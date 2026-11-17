// SPDX-License-Identifier: CC0-1.0

//! `DescribeInstancePatchStates` end to end: the high-level patch compliance state of the
//! managed nodes a request NAMES, in ascending node-id order and served one page at a time.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.DescribeInstancePatchStates` and an `awsJson1_1` body,
//! dispatched by the multi-service router (`OPERATIONS.md`, "Wire protocol").
//!
//! The reports themselves are seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_instance_patch_state`, the one store this
//! read projects), because patch compliance is gathered ON a node by the patching operation
//! Patch Manager runs there and no operation of this service's model reports one — the same
//! reason the sibling patch-group suite seeds that store directly. Seeding is what lets
//! these cases prove the response is computed from persisted state: what was reported for a
//! node is what a read of that node reports, per Region, and nothing else.
//!
//! The suites are tables: each row is one input along the axis the rule under test
//! generalises over (which ids are named, how many, their order, how often, Region, page
//! size, token), rather than the single list the recorded unit happened to send.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::instance_patch_states::{
    InstancePatchStateRecord, OPERATION_INSTALL, OPERATION_SCAN,
};
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for unless a case says otherwise. Deliberately not the
/// emulator's default, so a report read here proves the Region came from the request's
/// SigV4 credential scope.
const REGION: &str = "us-west-2";

/// A second Region, to pin that a report is read only through the Region its node is
/// registered with.
const OTHER_REGION: &str = "eu-central-1";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// The operation under test.
const OPERATION: &str = "DescribeInstancePatchStates";

/// The patch group the seeded nodes belong to. This read names nodes rather than a group, so
/// a group is only ever something a report CARRIES here.
const GROUP: &str = "production";

/// A second patch group, to pin that this read reaches a node's report whatever group the
/// node belongs to.
const OTHER_GROUP: &str = "staging fleet";

/// A baseline id of the documented spelling (`pb-` and 17 hex digits), which is also the
/// floor of the modelled `BaselineId` length of 20..128.
const BASELINE: &str = "pb-0123456789abcdef0";

/// A node whose report most cases read (an EC2 node id with the 8-character suffix the
/// modelled `InstanceId` pattern admits).
const NODE_A: &str = "i-00000001";

/// A second reported node, ordered after [`NODE_A`] by node id, in another patch group.
const NODE_B: &str = "i-00000002";

/// A third reported node, a hybrid (`mi-`) node id — ordered after both EC2 ids.
const NODE_C: &str = "mi-00000000000000001";

/// A node id this endpoint holds no report for: well formed, so the boundary walk admits it,
/// and nothing was ever gathered for it.
const UNREPORTED_NODE: &str = "i-0abcdef0123456789";

/// When the patching operations these cases seed ran, as a fixed instant: a report carries
/// the times its operation ran, so the case pins them rather than reading the clock.
fn ran_at() -> (SystemTime, SystemTime) {
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    (started, started + Duration::from_secs(97))
}

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case seeds patch states through.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope below, which is where the Region a report is read through comes
    // from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Record `report` in `region` the way a patching operation on the node would.
fn report_state(
    engine: &Arc<engine::InMemoryEngine>,
    region: &str,
    report: InstancePatchStateRecord,
) {
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_instance_patch_state(region, report),
        "a seeded report must carry the node id it is stored under"
    );
}

/// How many reports the store holds, to pin that a read wrote nothing.
fn stored_count(engine: &Arc<engine::InMemoryEngine>) -> usize {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .instance_patch_states
        .len()
}

/// Call the operation signed for `region`, returning `(status, body)`.
async fn call_in_region(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{OPERATION}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{region}/ssm/aws4_request, \
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

/// Call the operation signed for [`REGION`].
async fn call(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call_in_region(router, REGION, body).await
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

/// The node ids a successful read lists, in the order it listed them.
fn reported_ids(body: &Value) -> Vec<String> {
    body["InstancePatchStates"]
        .as_array()
        .expect("a read carries InstancePatchStates")
        .iter()
        .map(|state| {
            state["InstanceId"]
                .as_str()
                .expect("every reported state carries an InstanceId")
                .to_string()
        })
        .collect()
}

/// A read that must have succeeded, with the node ids it reported.
async fn read(router: &MultiServiceRouter, case: &str, body: &Value) -> (Value, Vec<String>) {
    let (status, parsed) = call(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {parsed:?}"
    );
    let ids = reported_ids(&parsed);
    (parsed, ids)
}

/// A read naming `requested` and nothing else.
fn naming(requested: &[&str]) -> Value {
    json!({ "InstanceIds": requested })
}

/// The ids of `expected`, as a read reports them.
fn ids(expected: &[&str]) -> Vec<String> {
    expected.iter().map(|id| (*id).to_string()).collect()
}

/// Three reports in [`REGION`] — two patch groups, both patching operations, counts spanning
/// the ones the model defaults to `0` and the ones it defaults to `null` — and a fourth node
/// reported only in [`OTHER_REGION`].
fn seed_mixed_reports(engine: &Arc<engine::InMemoryEngine>) {
    let (started, ended) = ran_at();

    let mut first =
        InstancePatchStateRecord::reported(NODE_A, GROUP, BASELINE, OPERATION_SCAN, started, ended);
    first.installed_count = 5;
    first.installed_other_count = 2;
    first.failed_count = 1;
    first.not_applicable_count = 3;
    first.installed_pending_reboot_count = Some(4);
    report_state(engine, REGION, first);

    let mut second = InstancePatchStateRecord::reported(
        NODE_B,
        OTHER_GROUP,
        BASELINE,
        OPERATION_INSTALL,
        started,
        ended,
    );
    second.missing_count = 7;
    second.unreported_not_applicable_count = Some(9);
    report_state(engine, REGION, second);

    let mut third =
        InstancePatchStateRecord::reported(NODE_C, GROUP, BASELINE, OPERATION_SCAN, started, ended);
    third.installed_count = 1;
    third.missing_count = 2;
    report_state(engine, REGION, third);

    // The same node id would be a second report were it in this Region; it is not.
    report_state(
        engine,
        OTHER_REGION,
        InstancePatchStateRecord::reported(
            UNREPORTED_NODE,
            GROUP,
            BASELINE,
            OPERATION_SCAN,
            started,
            ended,
        ),
    );
}

/// `count` reports in [`REGION`], node ids ascending, each with its index as its
/// `InstalledCount`; answers the ids it seeded.
fn seed_sequence(engine: &Arc<engine::InMemoryEngine>, count: usize) -> Vec<String> {
    let (started, ended) = ran_at();
    (1..=count)
        .map(|index| {
            let id = format!("i-{index:08}");
            let mut report = InstancePatchStateRecord::reported(
                &id,
                GROUP,
                BASELINE,
                OPERATION_SCAN,
                started,
                ended,
            );
            report.installed_count = i32::try_from(index).expect("a small index");
            report_state(engine, REGION, report);
            id
        })
        .collect()
}

#[tokio::test]
async fn a_read_reports_the_named_nodes_that_have_been_patched_and_no_other() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);
    let before = stored_count(&engine);

    // One row per shape a list of ids can take: one id, several, the same id twice, an id
    // nothing was gathered for, a mix of reported and unreported, and the empty list the
    // modelled `InstanceIdList` length of 0..50 admits. The reports come back in ascending
    // node-id order whatever order the request named them in.
    let cases: &[(&str, &[&str], &[&str])] = &[
        ("one reported node", &[NODE_A], &[NODE_A]),
        (
            "two reported nodes, named in descending order",
            &[NODE_B, NODE_A],
            &[NODE_A, NODE_B],
        ),
        (
            "the hybrid node id, which orders after the EC2 ids",
            &[NODE_C, NODE_A],
            &[NODE_A, NODE_C],
        ),
        (
            "every reported node, named out of order",
            &[NODE_C, NODE_B, NODE_A],
            &[NODE_A, NODE_B, NODE_C],
        ),
        (
            "the same node named twice, which holds one report",
            &[NODE_A, NODE_A],
            &[NODE_A],
        ),
        (
            "a node nothing has been gathered for",
            &[UNREPORTED_NODE],
            &[],
        ),
        (
            "a reported node beside an unreported one",
            &[UNREPORTED_NODE, NODE_B],
            &[NODE_B],
        ),
        ("no ids at all", &[], &[]),
    ];

    for (case, requested, expected) in cases {
        let (body, reported) = read(&router, case, &naming(requested)).await;
        assert_eq!(
            reported,
            ids(expected),
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
        let token = body["NextToken"].as_str();
        assert_eq!(
            token, None,
            "case {case:?}: a page that holds every report carries no token, got {token:?}"
        );
    }

    // A read reports; it must not change what a later caller sees.
    let after = stored_count(&engine);
    assert_eq!(
        after, before,
        "case \"read twice\": a read persists nothing, expected {before} reports, got {after}"
    );
    let (_, again) = read(&router, "read twice", &naming(&[NODE_C, NODE_B, NODE_A])).await;
    assert_eq!(
        again,
        ids(&[NODE_A, NODE_B, NODE_C]),
        "case \"read twice\": the second read reports the same, got {again:?}"
    );
}

#[tokio::test]
async fn a_report_is_reached_only_through_the_region_its_node_is_registered_with() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);

    // One row per Region the request can be signed for: the node reported in the other
    // Region is reachable there and nowhere else, and the nodes of this Region likewise.
    let cases: &[(&str, &[&str], &[&str])] = &[
        (REGION, &[NODE_A, UNREPORTED_NODE], &[NODE_A]),
        (OTHER_REGION, &[NODE_A, UNREPORTED_NODE], &[UNREPORTED_NODE]),
        (REGION, &[NODE_B, NODE_C], &[NODE_B, NODE_C]),
        (OTHER_REGION, &[NODE_B, NODE_C], &[]),
    ];

    for (region, requested, expected) in cases {
        let case = format!("signed for {region} naming {requested:?}");
        let (status, body) = call_in_region(&router, region, &naming(requested)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        let reported = reported_ids(&body);
        assert_eq!(
            reported,
            ids(expected),
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_report_is_projected_from_what_the_patching_operation_persisted() {
    let (router, engine) = service();
    let (started, ended) = ran_at();
    let mut report = InstancePatchStateRecord::reported(
        NODE_A,
        GROUP,
        BASELINE,
        OPERATION_INSTALL,
        started,
        ended,
    );
    report.snapshot_id = Some("521c3536-930c-4aa9-950e-01803194c6cc".to_string());
    report.install_override_list = Some("s3://patch-lists/override.yaml".to_string());
    report.reboot_option = Some("RebootIfNeeded".to_string());
    report.installed_count = 11;
    report.installed_other_count = 12;
    report.installed_pending_reboot_count = Some(13);
    report.installed_rejected_count = Some(14);
    report.missing_count = 15;
    report.failed_count = 16;
    report.unreported_not_applicable_count = Some(17);
    report.not_applicable_count = 18;
    report.available_security_update_count = Some(19);
    report.critical_non_compliant_count = Some(20);
    report.security_non_compliant_count = Some(21);
    report.other_non_compliant_count = Some(22);
    report.last_no_reboot_install_operation_time = Some(ended);
    report_state(&engine, REGION, report);

    let (body, reported) = read(&router, "every member", &naming(&[NODE_A])).await;
    assert_eq!(
        reported,
        ids(&[NODE_A]),
        "case \"every member\": expected the one seeded report, got {reported:?}"
    );
    let state = &body["InstancePatchStates"][0];

    let strings: &[(&str, &str)] = &[
        ("InstanceId", NODE_A),
        ("PatchGroup", GROUP),
        ("BaselineId", BASELINE),
        ("SnapshotId", "521c3536-930c-4aa9-950e-01803194c6cc"),
        ("InstallOverrideList", "s3://patch-lists/override.yaml"),
        ("Operation", OPERATION_INSTALL),
        ("RebootOption", "RebootIfNeeded"),
    ];
    for (member, expected) in strings {
        let actual = state[member].as_str();
        assert_eq!(
            actual,
            Some(*expected),
            "case {member:?}: expected {expected:?}, got {actual:?}"
        );
    }

    let counts: &[(&str, i64)] = &[
        ("InstalledCount", 11),
        ("InstalledOtherCount", 12),
        ("InstalledPendingRebootCount", 13),
        ("InstalledRejectedCount", 14),
        ("MissingCount", 15),
        ("FailedCount", 16),
        ("UnreportedNotApplicableCount", 17),
        ("NotApplicableCount", 18),
        ("AvailableSecurityUpdateCount", 19),
        ("CriticalNonCompliantCount", 20),
        ("SecurityNonCompliantCount", 21),
        ("OtherNonCompliantCount", 22),
    ];
    for (member, expected) in counts {
        let actual = state[member].as_i64();
        assert_eq!(
            actual,
            Some(*expected),
            "case {member:?}: expected {expected:?}, got {actual:?}"
        );
    }

    let timestamps: &[(&str, u64)] = &[
        ("OperationStartTime", 1_700_000_000),
        ("OperationEndTime", 1_700_000_097),
        ("LastNoRebootInstallOperationTime", 1_700_000_097),
    ];
    for (member, expected) in timestamps {
        let actual = state[member].as_f64();
        assert_eq!(
            actual,
            Some(*expected as f64),
            "case {member:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // A count no patching operation reported is reported by nobody: the member is absent
    // rather than carrying a figure this read invented.
    let unreported = &state["OwnerInformation"];
    assert!(
        unreported.is_null(),
        "case \"unreported member\": expected no OwnerInformation, got {unreported:?}"
    );
}

#[tokio::test]
async fn a_page_carries_what_was_asked_for_and_its_token_resumes_where_it_stopped() {
    let (router, engine) = service();
    // More reports than the smallest page the modelled `MaxResults` admits, so a page size
    // at that floor leaves a remainder to resume into.
    let seeded = seed_sequence(&engine, 23);
    let named: Vec<&str> = seeded.iter().map(String::as_str).collect();

    // One row per page size: the modelled floor and ceiling of `MaxResults` (range 10..100),
    // a size between them, one that divides the report count exactly, and no `MaxResults` at
    // all. Whatever the size, walking the tokens reports every named node exactly once in
    // ascending node-id order.
    let cases: &[(&str, Option<i32>, usize)] = &[
        ("the modelled floor", Some(10), 10),
        ("a size between the bounds", Some(12), 12),
        ("a size that divides the reports exactly", Some(23), 23),
        ("the modelled ceiling", Some(100), 100),
        ("no MaxResults at all", None, 100),
    ];

    for (case, max_results, expected_page) in cases {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut body = json!({ "InstanceIds": named });
            if let Some(max_results) = max_results {
                body["MaxResults"] = json!(max_results);
            }
            if let Some(token) = &token {
                body["NextToken"] = json!(token);
            }
            let (page, reported) = read(&router, case, &body).await;
            pages += 1;
            let remaining = seeded.len() - walked.len();
            let expected_this_page = remaining.min(*expected_page);
            assert_eq!(
                reported.len(),
                expected_this_page,
                "case {case:?}: page {pages} expected {expected_this_page} reports, got {}",
                reported.len()
            );
            walked.extend(reported);
            token = page["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                pages <= seeded.len(),
                "case {case:?}: paging must terminate, got {pages} pages for {} reports",
                seeded.len()
            );
        }
        assert_eq!(
            walked, seeded,
            "case {case:?}: expected every seeded report once, ascending, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_page_size_outside_the_modelled_range_is_refused() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);

    // The published bounds of `PatchComplianceMaxResults` (range 10..100) and one past each:
    // a page size the model admits is served, one outside it is refused as the validation of
    // the request it is.
    let cases: &[(&str, i32, u16)] = &[
        ("one below the floor", 9, 400),
        ("the floor", 10, 200),
        ("the ceiling", 100, 200),
        ("one past the ceiling", 101, 400),
    ];

    for (case, max_results, expected_status) in cases {
        let body = json!({ "InstanceIds": [NODE_A], "MaxResults": max_results });
        let (status, parsed) = call(&router, &body).await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with {parsed:?}"
        );
        if *expected_status == 400 {
            let code = error_code(&parsed);
            assert_eq!(
                code, "ValidationException",
                "case {case:?}: expected a ValidationException, got {code:?} with {parsed:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_next_token_this_read_did_not_hand_out_is_refused() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);

    // One row per shape a token can take: the empty string this read spells "no token" with,
    // a well-formed node id (what a token IS here), and three values no page of this read
    // ever handed out — an offset, a cursor of another read's spelling, and a node id with a
    // suffix the modelled `InstanceId` pattern does not admit.
    let cases: &[(&str, &str, u16)] = &[
        ("the empty string", "", 200),
        ("a node id this read could have handed out", NODE_B, 200),
        ("an offset", "3", 400),
        ("an opaque cursor", "eyJvZmZzZXQiOjN9", 400),
        ("a node id with a suffix of the wrong length", "i-0001", 400),
    ];

    for (case, token, expected_status) in cases {
        let body = json!({ "InstanceIds": [NODE_A, NODE_B, NODE_C], "NextToken": token });
        let (status, parsed) = call(&router, &body).await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with {parsed:?}"
        );
        if *expected_status == 400 {
            let code = error_code(&parsed);
            assert_eq!(
                code, "InvalidNextToken",
                "case {case:?}: expected an InvalidNextToken, got {code:?} with {parsed:?}"
            );
            let message = parsed["Message"].as_str().or(parsed["message"].as_str());
            assert_eq!(
                message,
                Some("The NextToken value is not valid"),
                "case {case:?}: expected the service's own wording, got {message:?}"
            );
        }
    }

    // A token this read could have handed out resumes at it: the reports before it are
    // behind the caller, the ones from it on are what the page carries.
    let (_, resumed) = read(
        &router,
        "resuming at the second node",
        &json!({ "InstanceIds": [NODE_A, NODE_B, NODE_C], "NextToken": NODE_B }),
    )
    .await;
    assert_eq!(
        resumed,
        ids(&[NODE_B, NODE_C]),
        "case \"resuming at the second node\": expected the reports from it on, got {resumed:?}"
    );
}
