// SPDX-License-Identifier: CC0-1.0

//! Patch Manager end to end: sending the patch-baseline document Systems Manager publishes
//! to a managed node, and the compliance that run reports back through the patch reads.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol").
//!
//! The node inventory is seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store every
//! read of this service projects), because a node enters it only by REGISTERING and no
//! public API of this service registers one. Everything else a case needs — the patch
//! baseline, which baseline is the default, the patching operation itself and the
//! compliance it reports — is done through the API, so the figures asserted here are
//! computed from what the emulator persisted rather than seeded.
//!
//! The suites are tables: each row is one input along the axis the rule under test
//! generalises over (the node's platform and so its operating system, the patching
//! operation, the reboot option, the patch group, which baseline is in force), rather than
//! the single send the recorded unit happened to make.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for, which is where a node, a baseline and a report are
/// reached through. Deliberately not the emulator's default, so what is read here proves
/// the Region came from the request's SigV4 credential scope.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// The Command document Patch Manager publishes, which exists in every account and Region
/// without anybody creating it (`state::patch_runs::RUN_PATCH_BASELINE_DOCUMENT`).
const PATCH_DOCUMENT: &str = "AWS-RunPatchBaseline";

/// The step of that document that performs the patching operation, which is what the
/// invocation reports as its `PluginName` (the document's `mainSteps[].name`).
const PATCH_STEP: &str = "PatchBaseline";

/// An online EC2 node running Linux.
const LINUX: &str = "i-0abc1234";

/// A second online EC2 node running Linux, in the same patch group as [`LINUX`].
const LINUX_TWO: &str = "i-0abc5678";

/// An online EC2 node running Windows.
const WINDOWS: &str = "i-0123456789abcdef0";

/// A node in another patch group, so a group read reaches only its own group's reports.
const OTHER_GROUP_NODE: &str = "i-0dddd111";

/// A node carrying no patch-group tag at all, which is patched all the same.
const UNGROUPED: &str = "i-0eeee222";

/// The patch group most cases read, as the value of the node's patch-group tag.
const GROUP: &str = "production";

/// A second patch group.
const OTHER_GROUP: &str = "staging fleet";

/// The tag key whose value names a node's patch group ("the tag key must be `Patch Group`
/// or `PatchGroup`", AWS Systems Manager User Guide).
const PATCH_GROUP_TAG: &str = "Patch Group";

/// The other spelling of that tag key, which names the same group.
const PATCH_GROUP_TAG_COMPACT: &str = "PatchGroup";

/// The `OperatingSystem` a Linux node is patched against here: a managed node reports no
/// `PlatformName`, and nothing in this suite's engine holds an Amazon EC2 image to read a
/// distribution off, so the Linux nodes are patched against the Amazon Linux release
/// `state::patch_runs::FALLBACK_LINUX_OPERATING_SYSTEM` names.
const LINUX_OPERATING_SYSTEM: &str = "AMAZON_LINUX_2023";

/// The `OperatingSystem` a Windows node is patched against.
const WINDOWS_OPERATING_SYSTEM: &str = "WINDOWS";

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
    // The error shape travels in `__type`; fold in the header the framework also sets so
    // one accessor reads either.
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// Call `operation`, asserting it succeeded.
async fn call_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
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

/// One registered node: its id, platform and tags.
fn node(id: &str, platform: &str, tags: &[(&str, &str)]) -> ManagedNodeRecord {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.properties.platform_type = Some(platform.to_string());
    record.properties.ping_status = Some("Online".to_string());
    record.properties.computer_name = Some(format!("{id}.compute.internal"));
    record.tags = tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect::<BTreeMap<String, String>>();
    record
}

/// Register `record` the way a node's own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, record: ManagedNodeRecord) {
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// Move the instant the clock owes `command_id`'s invocations their terminal status to now,
/// so a case about WHAT the run did does not wait out the span — the same move this crate's
/// other clock-owed suites make through the store rather than sleeping for it.
fn finish_due(engine: &Arc<engine::InMemoryEngine>, command_id: &str) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let command = store
        .commands
        .get_mut(&(REGION.to_string(), command_id.to_string()))
        .unwrap_or_else(|| panic!("the store must hold the command {command_id}"));
    for invocation in command.invocations.values_mut() {
        if let Some(outcome) = invocation.outcome.as_mut() {
            outcome.due_at = SystemTime::now();
        }
    }
}

/// A patch baseline of `operating_system` that approves every patch published for it on the
/// day it was released — "the filter values can be exact values for the patch property
/// given as a key, or a wildcard (*), which matches all values"
/// (`smithy_query(query="shape", shape="PatchFilter")`) — and the id it was created with.
async fn baseline_approving_everything(
    router: &MultiServiceRouter,
    case: &str,
    operating_system: &str,
    compliance_level: &str,
) -> String {
    let created = call_ok(
        router,
        case,
        "CreatePatchBaseline",
        &json!({
            "Name": format!("Runs.{operating_system}"),
            "OperatingSystem": operating_system,
            "ApprovalRules": {
                "PatchRules": [{
                    "PatchFilterGroup": { "PatchFilters": [{ "Key": "PRODUCT", "Values": ["*"] }] },
                    "ApproveAfterDays": 0,
                    "ComplianceLevel": compliance_level,
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

/// Register `baseline_id` as the default patch baseline of its operating system, which is
/// what makes the nodes of that operating system be patched against it.
async fn register_default(router: &MultiServiceRouter, case: &str, baseline_id: &str) {
    call_ok(
        router,
        case,
        "RegisterDefaultPatchBaseline",
        &json!({ "BaselineId": baseline_id }),
    )
    .await;
}

/// Send the published patch-baseline document to `instance_id` with the parameters
/// `parameters` names, returning `(status, body)`.
async fn run_patch_baseline(
    router: &MultiServiceRouter,
    instance_id: &str,
    parameters: &Value,
) -> (u16, Value) {
    call(
        router,
        "SendCommand",
        &json!({
            "DocumentName": PATCH_DOCUMENT,
            "InstanceIds": [instance_id],
            "Parameters": parameters,
        }),
    )
    .await
}

/// Send it for the operation `operation`, asserting the send was accepted, and answer the
/// command id it minted.
async fn scan_or_install(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    parameters: &Value,
) -> String {
    let (status, parsed) = run_patch_baseline(router, instance_id, parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a send of {PATCH_DOCUMENT} must be accepted, got {status} with \
         {parsed:?}"
    );
    parsed["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a send reports a CommandId, got {parsed}"))
        .to_string()
}

/// The high-level patch compliance report of one node, or `Value::Null` when this endpoint
/// holds none for it.
async fn patch_state(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Value {
    let read = call_ok(
        router,
        case,
        "DescribeInstancePatchStates",
        &json!({ "InstanceIds": [instance_id] }),
    )
    .await;
    read["InstancePatchStates"]
        .as_array()
        .and_then(|states| states.first().cloned())
        .unwrap_or(Value::Null)
}

/// Every patch row this endpoint holds for one node, following the read's own token so the
/// whole report is compared rather than its first page.
async fn patch_rows(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    filters: &Value,
) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({ "InstanceId": instance_id, "MaxResults": 10 });
        if !filters.is_null() {
            body["Filters"] = filters.clone();
        }
        if let Some(token) = &token {
            body["NextToken"] = json!(token);
        }
        let read = call_ok(router, case, "DescribeInstancePatches", &body).await;
        rows.extend(read["Patches"].as_array().cloned().unwrap_or_default());
        token = read["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            rows.len() < 10_000,
            "case {case:?}: the paged read must terminate, got {} rows",
            rows.len()
        );
    }
    rows
}

/// The aggregated state of one patch group.
async fn group_state(router: &MultiServiceRouter, case: &str, group: &str) -> Value {
    call_ok(
        router,
        case,
        "DescribePatchGroupState",
        &json!({ "PatchGroup": group }),
    )
    .await
}

/// An `i32` member of a reported state, which the counts are.
fn count(case: &str, body: &Value, member: &str) -> i64 {
    body[member]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} to be a count, got {body}"))
}

/// The `State` of every row, in the order reported.
fn states(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| row["State"].as_str().unwrap_or("<unset>").to_string())
        .collect()
}

/// A send of the published patch-baseline document is accepted on a fresh emulator — no
/// create of any kind — and the patching operation it starts reports the compliance of the
/// node against the baseline in force, patch by patch and as the high-level counts.
#[tokio::test]
async fn a_patch_baseline_run_reports_the_compliance_of_the_node() {
    // One row per operating system a node's platform resolves to, which is the axis the
    // baseline in force and the published patch catalogue both turn on.
    let rows: &[(&str, &str, &str, &str)] = &[
        ("a Linux node", LINUX, LINUX_OPERATING_SYSTEM, GROUP),
        (
            "a Windows node",
            WINDOWS,
            WINDOWS_OPERATING_SYSTEM,
            OTHER_GROUP,
        ),
    ];

    for (case, instance_id, operating_system, group) in rows {
        let (router, engine) = service();
        register(
            &engine,
            node(
                instance_id,
                if *operating_system == WINDOWS_OPERATING_SYSTEM {
                    "Windows"
                } else {
                    "Linux"
                },
                &[(PATCH_GROUP_TAG, group)],
            ),
        );

        // Nothing has been patched yet, so this endpoint holds no report for the node.
        assert_eq!(
            patch_state(&router, case, instance_id).await,
            Value::Null,
            "case {case:?}: a node nothing has patched has no report"
        );

        let baseline = baseline_approving_everything(&router, case, operating_system, "CRITICAL")
            .await;
        register_default(&router, case, &baseline).await;

        let command =
            scan_or_install(&router, case, instance_id, &json!({ "Operation": ["Scan"] })).await;
        assert!(
            !command.is_empty(),
            "case {case:?}: a send mints a command id"
        );

        let report = patch_state(&router, case, instance_id).await;
        assert_eq!(
            report["InstanceId"].as_str(),
            Some(*instance_id),
            "case {case:?}: the report is the node's own, got {report}"
        );
        assert_eq!(
            report["BaselineId"].as_str(),
            Some(baseline.as_str()),
            "case {case:?}: the run is performed against the default baseline of the node's \
             operating system, got {report}"
        );
        assert_eq!(
            report["PatchGroup"].as_str(),
            Some(*group),
            "case {case:?}: the report carries the group the node's tag names, got {report}"
        );
        assert_eq!(
            report["Operation"].as_str(),
            Some("Scan"),
            "case {case:?}: a scan reports the operation it performed, got {report}"
        );
        assert_eq!(
            report["RebootOption"].as_str(),
            Some("RebootIfNeeded"),
            "case {case:?}: a send naming no reboot option runs with the document's default, \
             got {report}"
        );

        // The patch rows ARE the report: the counts are counted from them, so a count and
        // the rows behind it cannot disagree.
        let patches = patch_rows(&router, case, instance_id, &Value::Null).await;
        let missing = count(case, &report, "MissingCount");
        assert!(
            missing > 0,
            "case {case:?}: a baseline approving every published patch leaves them all \
             missing on a node that holds none, got {report}"
        );
        assert_eq!(
            missing,
            i64::try_from(patches.len()).unwrap_or(i64::MAX),
            "case {case:?}: MissingCount counts the rows reported missing, got {} rows and \
             {report}",
            patches.len()
        );
        assert_eq!(
            count(case, &report, "InstalledCount"),
            0,
            "case {case:?}: nothing has installed a patch on this node, got {report}"
        );
        assert_eq!(
            count(case, &report, "CriticalNonCompliantCount"),
            missing,
            "case {case:?}: every missing patch is one the baseline assigns CRITICAL, got \
             {report}"
        );
        assert!(
            states(&patches).iter().all(|state| state == "MISSING"),
            "case {case:?}: every row of a scan that found nothing installed is MISSING, got \
             {:?}",
            states(&patches)
        );
        for row in &patches {
            for member in ["Title", "KBId", "State"] {
                assert!(
                    row[member].as_str().is_some_and(|value| !value.is_empty()),
                    "case {case:?}: every row carries {member}, got {row}"
                );
            }
            assert_eq!(
                row["InstalledTime"].as_f64(),
                Some(0.0),
                "case {case:?}: a patch that is not installed carries no installation \
                 instant, got {row}"
            );
        }

        // The invocation the send minted names the step that performed the run, once the
        // move off `Pending` the clock owes it has fallen due — moved through the store
        // here rather than waited out, the way this crate's other clock-owed suites do.
        finish_due(&engine, &command);
        let invocation = call_ok(
            &router,
            case,
            "GetCommandInvocation",
            &json!({ "CommandId": command, "InstanceId": instance_id }),
        )
        .await;
        assert_eq!(
            invocation["PluginName"].as_str(),
            Some(PATCH_STEP),
            "case {case:?}: the invocation names the step that ran, got {invocation}"
        );
        assert_eq!(
            invocation["ResponseCode"].as_i64(),
            Some(0),
            "case {case:?}: the step that performed the run succeeded, got {invocation}"
        );
        assert_eq!(
            invocation["Status"].as_str(),
            Some("Success"),
            "case {case:?}: the invocation finished, got {invocation}"
        );
    }
}

/// An `Install` run installs the patches the baseline approves, a later run reports them
/// installed rather than missing, and the reboot option the send named decides whether a
/// patch it installed is still waiting for a reboot.
#[tokio::test]
async fn an_install_run_installs_the_approved_patches() {
    // One row per reboot option, which is the axis the state of a newly installed patch
    // turns on.
    let rows: &[(&str, Value, &str, &str)] = &[
        (
            "an install that may reboot",
            json!({ "Operation": ["Install"] }),
            "INSTALLED",
            "InstalledCount",
        ),
        (
            "an install that may not reboot",
            json!({ "Operation": ["Install"], "RebootOption": ["NoReboot"] }),
            "INSTALLED_PENDING_REBOOT",
            "InstalledPendingRebootCount",
        ),
    ];

    for (case, parameters, expected_state, expected_count) in rows {
        let (router, engine) = service();
        register(&engine, node(LINUX, "Linux", &[(PATCH_GROUP_TAG, GROUP)]));
        let baseline =
            baseline_approving_everything(&router, case, LINUX_OPERATING_SYSTEM, "HIGH").await;
        register_default(&router, case, &baseline).await;

        // What the baseline approves, from the scan that found none of it installed.
        scan_or_install(&router, case, LINUX, &json!({ "Operation": ["Scan"] })).await;
        let scanned = patch_state(&router, case, LINUX).await;
        let approved = count(case, &scanned, "MissingCount");
        assert!(
            approved > 0,
            "case {case:?}: the scan must find missing patches to install, got {scanned}"
        );

        scan_or_install(&router, case, LINUX, parameters).await;
        let installed = patch_state(&router, case, LINUX).await;
        assert_eq!(
            installed["Operation"].as_str(),
            Some("Install"),
            "case {case:?}: the report is the install's, got {installed}"
        );
        assert_eq!(
            count(case, &installed, expected_count),
            approved,
            "case {case:?}: the install installed every approved patch, got {installed}"
        );
        assert_eq!(
            count(case, &installed, "MissingCount"),
            0,
            "case {case:?}: no approved patch is missing after the install, got {installed}"
        );
        assert!(
            states(&patch_rows(&router, case, LINUX, &Value::Null).await)
                .iter()
                .all(|state| state == expected_state),
            "case {case:?}: every row the install touched is {expected_state}, got {:?}",
            states(&patch_rows(&router, case, LINUX, &Value::Null).await)
        );
        assert_eq!(
            installed["LastNoRebootInstallOperationTime"].is_null(),
            *expected_state == "INSTALLED",
            "case {case:?}: only a run that may not reboot records a no-reboot install, got \
             {installed}"
        );

        // A later scan reports what the install left on the node: the report IS the state
        // of the node, so the patches stay installed without another install.
        scan_or_install(&router, case, LINUX, &json!({ "Operation": ["Scan"] })).await;
        let rescanned = patch_state(&router, case, LINUX).await;
        assert_eq!(
            count(case, &rescanned, expected_count),
            approved,
            "case {case:?}: a later scan still reports the installed patches, got {rescanned}"
        );
        assert_eq!(
            count(case, &rescanned, "MissingCount"),
            0,
            "case {case:?}: a later scan finds nothing missing, got {rescanned}"
        );
        assert_eq!(
            rescanned["Operation"].as_str(),
            Some("Scan"),
            "case {case:?}: the latest report is the latest operation's, got {rescanned}"
        );

        // A filter narrows the rows by the state they are in, and selects none for a state
        // no row is in.
        let filtered = patch_rows(
            &router,
            case,
            LINUX,
            &json!([{ "Key": "State", "Values": [expected_state] }]),
        )
        .await;
        assert_eq!(
            i64::try_from(filtered.len()).unwrap_or(i64::MAX),
            approved,
            "case {case:?}: a filter on the state the rows are in selects them all, got {} rows",
            filtered.len()
        );
        let none = patch_rows(
            &router,
            case,
            LINUX,
            &json!([{ "Key": "State", "Values": ["FAILED"] }]),
        )
        .await;
        assert!(
            none.is_empty(),
            "case {case:?}: no row of this run failed, got {none:?}"
        );
    }
}

/// The patch-group read aggregates the reports of the group's nodes: one count of NODES per
/// figure, over the nodes whose own report carries patches in that state, and nothing of
/// another group or of a group nothing has been patched in.
#[tokio::test]
async fn the_patch_group_state_aggregates_the_reports_of_its_nodes() {
    let case = "two nodes patched differently in one group";
    let (router, engine) = service();
    // Two nodes of the group, one of another group, and one carrying no patch-group tag —
    // and the compact spelling of the tag key on the second, which names the same group.
    register(&engine, node(LINUX, "Linux", &[(PATCH_GROUP_TAG, GROUP)]));
    register(
        &engine,
        node(LINUX_TWO, "Linux", &[(PATCH_GROUP_TAG_COMPACT, GROUP)]),
    );
    register(
        &engine,
        node(OTHER_GROUP_NODE, "Linux", &[(PATCH_GROUP_TAG, OTHER_GROUP)]),
    );
    register(&engine, node(UNGROUPED, "Linux", &[]));

    let baseline =
        baseline_approving_everything(&router, case, LINUX_OPERATING_SYSTEM, "CRITICAL").await;
    register_default(&router, case, &baseline).await;

    // One node scanned (its approved patches missing), one installed, one in the other
    // group, and the ungrouped one scanned too.
    scan_or_install(&router, case, LINUX, &json!({ "Operation": ["Scan"] })).await;
    scan_or_install(
        &router,
        case,
        LINUX_TWO,
        &json!({ "Operation": ["Install"] }),
    )
    .await;
    scan_or_install(
        &router,
        case,
        OTHER_GROUP_NODE,
        &json!({ "Operation": ["Scan"] }),
    )
    .await;
    scan_or_install(&router, case, UNGROUPED, &json!({ "Operation": ["Scan"] })).await;

    let aggregated = group_state(&router, case, GROUP).await;
    assert_eq!(
        count(case, &aggregated, "Instances"),
        2,
        "case {case:?}: the group holds the two nodes whose tag names it, got {aggregated}"
    );
    assert_eq!(
        count(case, &aggregated, "InstancesWithMissingPatches"),
        1,
        "case {case:?}: one node of the group has missing patches, got {aggregated}"
    );
    assert_eq!(
        count(case, &aggregated, "InstancesWithInstalledPatches"),
        1,
        "case {case:?}: one node of the group has installed patches, got {aggregated}"
    );
    assert_eq!(
        count(case, &aggregated, "InstancesWithCriticalNonCompliantPatches"),
        1,
        "case {case:?}: only the node with patches missing is non-compliant, got {aggregated}"
    );
    assert_eq!(
        count(case, &aggregated, "InstancesWithFailedPatches"),
        0,
        "case {case:?}: no node of the group failed to install a patch, got {aggregated}"
    );

    // The aggregate counts the same reports the per-node read of the group reports.
    let per_node = call_ok(
        &router,
        case,
        "DescribeInstancePatchStatesForPatchGroup",
        &json!({ "PatchGroup": GROUP }),
    )
    .await;
    let reported = per_node["InstancePatchStates"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        count(case, &aggregated, "Instances"),
        i64::try_from(reported.len()).unwrap_or(i64::MAX),
        "case {case:?}: the aggregate counts the reports the group read reports, got {per_node}"
    );
    assert_eq!(
        count(case, &aggregated, "InstancesWithMissingPatches"),
        i64::try_from(
            reported
                .iter()
                .filter(|report| report["MissingCount"].as_i64().unwrap_or_default() > 0)
                .count()
        )
        .unwrap_or(i64::MAX),
        "case {case:?}: the aggregate counts the nodes whose own report has patches missing, \
         got {per_node}"
    );

    // The other group holds its own node, and a group nothing has been patched in holds
    // none — which is what this endpoint knows, rather than a refusal.
    let other = group_state(&router, case, OTHER_GROUP).await;
    assert_eq!(
        count(case, &other, "Instances"),
        1,
        "case {case:?}: the other group holds its own node, got {other}"
    );
    let unpatched = group_state(&router, case, "a group nothing is in").await;
    for member in [
        "Instances",
        "InstancesWithMissingPatches",
        "InstancesWithInstalledPatches",
        "InstancesWithNotApplicablePatches",
    ] {
        assert_eq!(
            count(case, &unpatched, member),
            0,
            "case {case:?}: a group nothing has been patched in counts no node under \
             {member}, got {unpatched}"
        );
    }
    // The node carrying no patch-group tag was patched too, and belongs to no group.
    let ungrouped = patch_state(&router, case, UNGROUPED).await;
    assert_eq!(
        ungrouped["PatchGroup"].as_str(),
        Some(""),
        "case {case:?}: a node with no patch-group tag belongs to no group, got {ungrouped}"
    );
}

/// What a send of the patch document is refused for, and what a run against a baseline this
/// endpoint holds no approval configuration for reports.
#[tokio::test]
async fn the_refusals_and_the_boundary_of_a_patch_run() {
    let case = "a send naming no operation";
    let (router, engine) = service();
    register(&engine, node(LINUX, "Linux", &[(PATCH_GROUP_TAG, GROUP)]));

    // `Operation` is the one parameter the document declares no default for, so a send that
    // supplies none is missing a required parameter.
    let (status, refused) = run_patch_baseline(&router, LINUX, &json!({})).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the send to be refused, got {status} with {refused:?}"
    );
    assert_eq!(
        error_code(&refused),
        "InvalidParameters",
        "case {case:?}: expected InvalidParameters, got {refused}"
    );
    assert_eq!(
        patch_state(&router, case, LINUX).await,
        Value::Null,
        "case {case:?}: a refused send patches nothing"
    );

    // A parameter the document does not declare is refused the same way, whatever else the
    // send carries.
    let case = "a send naming a parameter the document does not declare";
    let (status, refused) = run_patch_baseline(
        &router,
        LINUX,
        &json!({ "Operation": ["Scan"], "NoSuchParameter": ["1"] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the send to be refused, got {status} with {refused:?}"
    );
    assert_eq!(
        error_code(&refused),
        "InvalidParameters",
        "case {case:?}: expected InvalidParameters, got {refused}"
    );

    // With no baseline of the account registered as the default, the node is patched
    // against the baseline Systems Manager predefines, whose approval configuration is
    // published with it: the run reports for the node, against that baseline, and the
    // patches that configuration approves are the ones it finds missing.
    let case = "a run against the predefined default baseline";
    scan_or_install(&router, case, LINUX, &json!({ "Operation": ["Scan"] })).await;
    let report = patch_state(&router, case, LINUX).await;
    assert_eq!(
        report["InstanceId"].as_str(),
        Some(LINUX),
        "case {case:?}: the run reported for the node, got {report}"
    );
    let predefined = call_ok(
        &router,
        case,
        "GetDefaultPatchBaseline",
        &json!({ "OperatingSystem": LINUX_OPERATING_SYSTEM }),
    )
    .await;
    // A predefined baseline is NAMED by its full ARN, because it is Systems Manager's
    // rather than the account's; the report records the baseline's own id.
    let named = predefined["BaselineId"].as_str().unwrap_or_default();
    assert!(
        named.ends_with(report["BaselineId"].as_str().unwrap_or("<unset>")),
        "case {case:?}: the run is performed against the default baseline, got {report} and \
         {predefined}"
    );
    // The patches it approves are a PROPER SUBSET of the catalogue it publishes for that
    // operating system — the rules select on the advisories' own classification and
    // severity — so the scan reports some of them missing and nothing installed.
    let missing = count(case, &report, "MissingCount");
    assert!(
        missing > 0,
        "case {case:?}: the predefined baseline's own rules approve patches of the published \
         catalogue, so a node holding none has them missing, got {report}"
    );
    assert_eq!(
        count(case, &report, "InstalledCount"),
        0,
        "case {case:?}: nothing has installed a patch on this node, got {report}"
    );
    let rows = patch_rows(&router, case, LINUX, &Value::Null).await;
    assert_eq!(
        i64::try_from(
            rows.iter()
                .filter(|row| row["State"].as_str() == Some("MISSING"))
                .count()
        )
        .unwrap_or(i64::MAX),
        missing,
        "case {case:?}: the rows behind the count are the patches it found missing, got {} rows",
        rows.len()
    );
    // What the run computed is what a read of the baseline itself reports: the approval
    // rules the predefined baseline publishes, which is what decided those rows.
    let baseline = call_ok(
        &router,
        case,
        "GetPatchBaseline",
        &json!({ "BaselineId": named }),
    )
    .await;
    assert!(
        baseline["ApprovalRules"]["PatchRules"]
            .as_array()
            .is_some_and(|rules| !rules.is_empty()),
        "case {case:?}: a predefined baseline reports the approval rules published with it, \
         got {baseline}"
    );
}

/// The compliance counts count the patches the baseline WANTS on the node and are not there:
/// a patch the baseline defines that is not to be installed on this node — explicitly
/// rejected, or not yet approved by the rule that selects it — is compliant, so it is
/// reported as not applicable and counts towards none of the non-compliant counts.
#[tokio::test]
async fn the_non_compliant_counts_count_only_what_the_baseline_wants_installed() {
    let (router, engine) = service();
    register(&engine, node(LINUX, "Linux", &[(PATCH_GROUP_TAG, GROUP)]));

    // What the catalogue holds for this operating system, taken from a scan against a
    // baseline that approves all of it at the critical compliance level.
    let case = "every approved patch missing";
    let approving = baseline_approving_everything(&router, case, LINUX_OPERATING_SYSTEM, "CRITICAL")
        .await;
    register_default(&router, case, &approving).await;
    scan_or_install(&router, case, LINUX, &json!({ "Operation": ["Scan"] })).await;
    let report = patch_state(&router, case, LINUX).await;
    let approved = count(case, &report, "MissingCount");
    assert!(
        approved > 1,
        "case {case:?}: the catalogue must hold more than one patch to reject one of, got \
         {report}"
    );
    assert_eq!(
        count(case, &report, "CriticalNonCompliantCount"),
        approved,
        "case {case:?}: every patch the baseline wants and the node lacks is critically \
         non-compliant, got {report}"
    );
    let rejected: Vec<String> = patch_rows(&router, case, LINUX, &Value::Null)
        .await
        .iter()
        .take(1)
        .map(|row| row["KBId"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        rejected.iter().all(|id| !id.is_empty()),
        "case {case:?}: a row names the patch it is about, got {rejected:?}"
    );

    // (case, the baseline the run is performed against, the states its rows carry)
    let rows: &[(&str, Value, &str)] = &[
        (
            "a patch the baseline rejects explicitly",
            json!({
                "Name": "Runs.Rejecting",
                "OperatingSystem": LINUX_OPERATING_SYSTEM,
                "ApprovalRules": {
                    "PatchRules": [{
                        "PatchFilterGroup": {
                            "PatchFilters": [{ "Key": "PRODUCT", "Values": ["*"] }],
                        },
                        "ApproveAfterDays": 0,
                        "ComplianceLevel": "CRITICAL",
                        "EnableNonSecurity": true,
                    }],
                },
                "RejectedPatches": rejected,
            }),
            "NOT_APPLICABLE",
        ),
        (
            "a rule whose approval window has not passed",
            json!({
                "Name": "Runs.Pending",
                "OperatingSystem": LINUX_OPERATING_SYSTEM,
                "ApprovalRules": {
                    "PatchRules": [{
                        "PatchFilterGroup": {
                            "PatchFilters": [{ "Key": "PRODUCT", "Values": ["*"] }],
                        },
                        // The widest window the member admits (`PatchRule$ApproveAfterDays`,
                        // range 0..360), so no patch of the catalogue is approved yet.
                        "ApproveAfterDays": 360,
                        "ComplianceLevel": "CRITICAL",
                        "EnableNonSecurity": true,
                    }],
                },
            }),
            "NOT_APPLICABLE",
        ),
    ];

    for (case, create, expected_state) in rows {
        let created = call_ok(&router, case, "CreatePatchBaseline", create).await;
        let baseline = created["BaselineId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a create reports a BaselineId"))
            .to_string();
        register_default(&router, case, &baseline).await;
        scan_or_install(&router, case, LINUX, &json!({ "Operation": ["Scan"] })).await;

        let report = patch_state(&router, case, LINUX).await;
        let not_applicable = count(case, &report, "NotApplicableCount");
        assert!(
            not_applicable > 0,
            "case {case:?}: the patches the baseline does not want installed are reported not \
             applicable, got {report}"
        );
        assert_eq!(
            count(case, &report, "MissingCount")
                + count(case, &report, "InstalledCount")
                + not_applicable,
            i64::try_from(patch_rows(&router, case, LINUX, &Value::Null).await.len())
                .unwrap_or(i64::MAX),
            "case {case:?}: every row is counted exactly once, got {report}"
        );
        assert_eq!(
            count(case, &report, "CriticalNonCompliantCount")
                + count(case, &report, "SecurityNonCompliantCount")
                + count(case, &report, "OtherNonCompliantCount"),
            count(case, &report, "MissingCount") + count(case, &report, "FailedCount"),
            "case {case:?}: only the patches the baseline wants and the node lacks are \
             non-compliant, got {report}"
        );
        let selected = patch_rows(
            &router,
            case,
            LINUX,
            &json!([{ "Key": "State", "Values": [expected_state] }]),
        )
        .await;
        assert_eq!(
            i64::try_from(selected.len()).unwrap_or(i64::MAX),
            not_applicable,
            "case {case:?}: the rows in {expected_state} are the ones the count counts, got {} \
             rows",
            selected.len()
        );
    }
}
