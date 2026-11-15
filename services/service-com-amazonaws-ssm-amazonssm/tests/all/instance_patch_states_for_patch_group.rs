// SPDX-License-Identifier: CC0-1.0

//! `DescribeInstancePatchStatesForPatchGroup` end to end: the high-level patch compliance
//! state the nodes of one patch group have reported, narrowed by the count filters the
//! read accepts and served one page at a time.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.DescribeInstancePatchStatesForPatchGroup` and an `awsJson1_1`
//! body, dispatched by the multi-service router (`OPERATIONS.md`, "Wire protocol").
//!
//! The reports themselves are seeded through the service's own state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_instance_patch_state`, the one store this
//! read projects), because patch compliance is gathered ON a node by the patching
//! operation Patch Manager runs there and no operation of this service's model reports one
//! — the same reason the sibling node suites seed the node store directly. Seeding is what
//! lets these cases prove the response is computed from persisted state: what was reported
//! is what is read back, per Region and per patch group, and nothing else.
//!
//! The suites are tables: each row is one input along the axis the rule under test
//! generalises over (patch group, Region, filter key, comparison, value, filter count,
//! page size, report count, token), rather than the single filter the recorded unit
//! happened to send.

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
const OTHER_REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// The operation under test.
const OPERATION: &str = "DescribeInstancePatchStatesForPatchGroup";

/// The patch group most cases read.
const GROUP: &str = "production";

/// A second patch group, to pin that a read reports the group it names and no other. The
/// modelled `PatchGroup` pattern `^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$` admits the space.
const OTHER_GROUP: &str = "staging fleet";

/// The same name as [`GROUP`] in another case, to pin that a patch group is named exactly.
const GROUP_IN_ANOTHER_CASE: &str = "Production";

/// A baseline id of the documented spelling (`pb-` and 17 hex digits), which is also the
/// floor of the modelled `BaselineId` length of 20..128.
const BASELINE: &str = "pb-0123456789abcdef0";

/// A node whose patch states most filter cases are about (an EC2 node id with the 8-
/// character suffix the modelled `InstanceId` pattern admits).
const NODE_A: &str = "i-00000001";

/// A second node of the same group, ordered after [`NODE_A`] by node id.
const NODE_B: &str = "i-00000002";

/// A third node of the same group, a hybrid (`mi-`) node id — ordered after both EC2 ids.
const NODE_C: &str = "mi-00000000000000001";

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
fn report_state(engine: &Arc<engine::InMemoryEngine>, region: &str, report: InstancePatchStateRecord) {
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

/// A read bounded by nothing but the patch group it names.
fn whole_group(group: &str) -> Value {
    json!({ "PatchGroup": group })
}

/// One `Filters` entry.
fn filter(key: &str, comparison: &str, value: &str) -> Value {
    json!({ "Key": key, "Values": [value], "Type": comparison })
}

/// Three reports of [`GROUP`] in [`REGION`], spanning every count a filter names: the
/// counts that carry a modelled default of `0` and the ones that default to `null`, which
/// one report leaves unreported and another carries a figure for. A fourth report sits in
/// another group and a fifth in another Region.
fn seed_mixed_reports(engine: &Arc<engine::InMemoryEngine>) {
    let (started, ended) = ran_at();

    let mut first =
        InstancePatchStateRecord::reported(NODE_A, GROUP, BASELINE, OPERATION_SCAN, started, ended);
    first.installed_count = 5;
    first.installed_other_count = 2;
    first.missing_count = 0;
    first.failed_count = 1;
    first.not_applicable_count = 3;
    first.installed_pending_reboot_count = Some(4);
    first.installed_rejected_count = Some(0);
    report_state(engine, REGION, first);

    let mut second = InstancePatchStateRecord::reported(
        NODE_B,
        GROUP,
        BASELINE,
        OPERATION_INSTALL,
        started,
        ended,
    );
    second.installed_count = 0;
    second.missing_count = 7;
    second.unreported_not_applicable_count = Some(9);
    report_state(engine, REGION, second);

    let mut third =
        InstancePatchStateRecord::reported(NODE_C, GROUP, BASELINE, OPERATION_SCAN, started, ended);
    third.installed_count = 5;
    third.installed_other_count = 1;
    third.missing_count = 7;
    third.failed_count = 2;
    third.not_applicable_count = 1;
    third.installed_pending_reboot_count = Some(0);
    third.installed_rejected_count = Some(3);
    third.unreported_not_applicable_count = Some(0);
    report_state(engine, REGION, third);

    // Same Region, another patch group: never reported through [`GROUP`].
    report_state(
        engine,
        REGION,
        InstancePatchStateRecord::reported(
            "i-00000009",
            OTHER_GROUP,
            BASELINE,
            OPERATION_SCAN,
            started,
            ended,
        ),
    );
    // Same group name, another Region: never reported through [`REGION`].
    report_state(
        engine,
        OTHER_REGION,
        InstancePatchStateRecord::reported(
            "i-00000008",
            GROUP,
            BASELINE,
            OPERATION_SCAN,
            started,
            ended,
        ),
    );
}

/// `count` reports of [`GROUP`], node ids ascending, each with its index as its
/// `InstalledCount`.
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

/// The ids of `expected`, as a read reports them.
fn ids(expected: &[&str]) -> Vec<String> {
    expected.iter().map(|id| (*id).to_string()).collect()
}

#[tokio::test]
async fn a_read_reports_the_patch_group_and_region_it_names_and_no_other() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);

    // One row per patch group reachable through [`REGION`], plus the same name in another
    // case and a group nothing has reported for: a group is named exactly, and a report
    // belongs to one group.
    let cases: &[(&str, &[&str])] = &[
        (GROUP, &[NODE_A, NODE_B, NODE_C]),
        (OTHER_GROUP, &["i-00000009"]),
        (GROUP_IN_ANOTHER_CASE, &[]),
        ("never-patched", &[]),
    ];

    for (group, expected) in cases {
        let case = format!("PatchGroup={group}");
        let (_, reported) = read(&router, &case, &whole_group(group)).await;
        assert_eq!(
            reported,
            ids(expected),
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }

    // The same group read through the other Region reports that Region's own report only.
    let (status, body) = call_in_region(&router, OTHER_REGION, &whole_group(GROUP)).await;
    assert_eq!(
        status, 200,
        "case \"other Region\": expected HTTP 200, got {status} with {body:?}"
    );
    let reported = reported_ids(&body);
    assert_eq!(
        reported,
        ids(&["i-00000008"]),
        "case \"other Region\": expected only that Region's report, got {reported:?}"
    );
}

#[tokio::test]
async fn a_count_filter_reports_the_states_its_comparison_holds_of() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);

    // One row per key the `InstancePatchStateFilter$Key` doc lists ("supported values
    // include the following"), crossed with the comparisons the modelled
    // `InstancePatchStateOperatorType` declares (Equal, NotEqual, LessThan, GreaterThan).
    // The two keys whose count the model defaults to `null` carry rows where one report
    // reported no figure at all: a count nothing reported is no number to compare, so no
    // comparison on it holds.
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("InstalledCount", "Equal", "5", &[NODE_A, NODE_C]),
        ("InstalledCount", "Equal", "0", &[NODE_B]),
        ("InstalledCount", "NotEqual", "5", &[NODE_B]),
        ("InstalledCount", "LessThan", "5", &[NODE_B]),
        ("InstalledCount", "GreaterThan", "0", &[NODE_A, NODE_C]),
        // A value larger than a count can hold is still an integer to compare against.
        ("InstalledCount", "GreaterThan", "9999999999", &[]),
        ("InstalledOtherCount", "Equal", "2", &[NODE_A]),
        ("InstalledOtherCount", "GreaterThan", "0", &[NODE_A, NODE_C]),
        ("MissingCount", "Equal", "7", &[NODE_B, NODE_C]),
        ("MissingCount", "LessThan", "7", &[NODE_A]),
        ("MissingCount", "GreaterThan", "7", &[]),
        ("FailedCount", "Equal", "1", &[NODE_A]),
        ("FailedCount", "NotEqual", "0", &[NODE_A, NODE_C]),
        ("FailedCount", "LessThan", "2", &[NODE_A, NODE_B]),
        ("NotApplicableCount", "GreaterThan", "0", &[NODE_A, NODE_C]),
        ("NotApplicableCount", "Equal", "0", &[NODE_B]),
        ("InstalledPendingRebootCount", "Equal", "4", &[NODE_A]),
        ("InstalledPendingRebootCount", "Equal", "0", &[NODE_C]),
        ("InstalledPendingRebootCount", "NotEqual", "4", &[NODE_C]),
        ("InstalledPendingRebootCount", "LessThan", "4", &[NODE_C]),
        ("InstalledRejectedCount", "GreaterThan", "0", &[NODE_C]),
        ("InstalledRejectedCount", "Equal", "0", &[NODE_A]),
        ("UnreportedNotApplicableCount", "Equal", "9", &[NODE_B]),
        ("UnreportedNotApplicableCount", "LessThan", "9", &[NODE_C]),
        ("UnreportedNotApplicableCount", "GreaterThan", "9", &[]),
    ];

    for (key, comparison, value, expected) in cases {
        let case = format!("Key={key} Type={comparison} Values=[{value}]");
        let body = json!({
            "PatchGroup": GROUP,
            "Filters": [filter(key, comparison, value)],
        });
        let (_, reported) = read(&router, &case, &body).await;
        assert_eq!(
            reported,
            ids(expected),
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn filters_narrow_together() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);

    // Rows differ in how many filters they carry (up to the modelled maximum of four) and
    // in whether the comparisons can hold of one report at once: a report is reported only
    // when EVERY filter is satisfied.
    let cases: &[(Value, &[&str])] = &[
        (
            json!([filter("InstalledCount", "Equal", "5")]),
            &[NODE_A, NODE_C],
        ),
        (
            json!([
                filter("InstalledCount", "Equal", "5"),
                filter("MissingCount", "Equal", "7"),
            ]),
            &[NODE_C],
        ),
        (
            json!([
                filter("InstalledCount", "Equal", "5"),
                filter("FailedCount", "Equal", "1"),
            ]),
            &[NODE_A],
        ),
        (
            json!([
                filter("InstalledCount", "Equal", "5"),
                filter("MissingCount", "Equal", "7"),
                filter("FailedCount", "GreaterThan", "1"),
                filter("InstalledRejectedCount", "Equal", "3"),
            ]),
            &[NODE_C],
        ),
        (
            json!([
                filter("InstalledCount", "Equal", "0"),
                filter("MissingCount", "Equal", "0"),
            ]),
            &[],
        ),
    ];

    for (filters, expected) in cases {
        let case = format!("Filters={filters}");
        let body = json!({ "PatchGroup": GROUP, "Filters": filters });
        let (_, reported) = read(&router, &case, &body).await;
        assert_eq!(
            reported,
            ids(expected),
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_filter_naming_no_patch_compliance_count_is_refused() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);

    // Rows differ in HOW the filter names nothing this read can narrow by: a key spelled
    // in another case, a key of another read's vocabulary, a key that is a prefix or a
    // decorated spelling of a real one, a comparison outside the modelled operator set,
    // and a value that is not the non-negative integer a count is compared against. The
    // last column is the message the refusal carries.
    let unknown_key = |key: &str| format!("Unknown Filter Key: {key}");
    let bad_value = |value: &str| {
        format!(
            "Invalid Filter Value: {value}. The value for the filter must be an integer \
             greater than or equal to 0"
        )
    };
    let cases: &[(&str, &str, &str, String)] = &[
        (
            "installedCount",
            "Equal",
            "1",
            unknown_key("installedCount"),
        ),
        (
            "INSTALLED_COUNT",
            "Equal",
            "1",
            unknown_key("INSTALLED_COUNT"),
        ),
        ("Installed", "Equal", "1", unknown_key("Installed")),
        (
            "InstalledCounts",
            "Equal",
            "1",
            unknown_key("InstalledCounts"),
        ),
        (
            "Installed Count",
            "Equal",
            "1",
            unknown_key("Installed Count"),
        ),
        ("NAME_PREFIX", "Equal", "1", unknown_key("NAME_PREFIX")),
        ("tag:env", "Equal", "1", unknown_key("tag:env")),
        (
            "InstalledCount",
            "Equal",
            "not-a-number",
            bad_value("not-a-number"),
        ),
        ("InstalledCount", "Equal", "-1", bad_value("-1")),
        ("InstalledCount", "Equal", "1.5", bad_value("1.5")),
        ("InstalledCount", "Equal", "0x1", bad_value("0x1")),
        ("MissingCount", "GreaterThan", " 1", bad_value(" 1")),
    ];

    for (key, comparison, value, expected) in cases {
        let case = format!("Key={key} Type={comparison} Values=[{value}]");
        let body = json!({
            "PatchGroup": GROUP,
            "Filters": [filter(key, comparison, value)],
        });
        let (status, parsed) = call(&router, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {parsed:?}"
        );
        let message = parsed["message"]
            .as_str()
            .or_else(|| parsed["Message"].as_str())
            .unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // A comparison outside the modelled `InstancePatchStateOperatorType` is refused by the
    // centralized boundary walk, before this read runs, with the clause this service words
    // every value outside an enumerated type with.
    for comparison in ["equal", "BeginWith"] {
        let case = format!("Type={comparison}");
        let body = json!({
            "PatchGroup": GROUP,
            "Filters": [filter("InstalledCount", comparison, "1")],
        });
        let (status, parsed) = call(&router, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {parsed:?}"
        );
        let message = parsed["message"].as_str().unwrap_or_default();
        let expected = format!(
            "1 validation error detected: Value '{comparison}' at 'filters.1.member.type' \
             failed to satisfy constraint: Member must satisfy enum value set: [NotEqual, \
             Equal, LessThan, GreaterThan]"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // A later filter of the same request is held to the same rule as the first.
    let body = json!({
        "PatchGroup": GROUP,
        "Filters": [
            filter("InstalledCount", "Equal", "1"),
            filter("NotACount", "Equal", "1"),
        ],
    });
    let (status, parsed) = call(&router, &body).await;
    let message = parsed["message"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        status, 400,
        "case \"second filter\": expected HTTP 400, got {status} with {parsed:?}"
    );
    assert_eq!(
        message,
        unknown_key("NotACount"),
        "case \"second filter\": expected the second filter refused, got {message:?}"
    );

    assert_eq!(
        stored_count(&engine),
        5,
        "a refused read must leave every report in place"
    );
}

#[tokio::test]
async fn a_page_is_bounded_by_the_modelled_result_list_and_resumes_from_its_token() {
    let (router, engine) = service();
    let seeded = seed_sequence(&engine, 7);

    // The result list holds at most five states (`InstancePatchStatesList`, len 1..5), and
    // every `MaxResults` the model admits (range 10..100) is above that, so each row is
    // served the same five and resumes at the sixth.
    let cases: &[Option<i32>] = &[None, Some(10), Some(50), Some(100)];

    for max_results in cases {
        let case = format!("MaxResults={max_results:?}");
        let mut body = whole_group(GROUP);
        if let Some(max_results) = max_results {
            body["MaxResults"] = json!(max_results);
        }
        let (first, reported) = read(&router, &case, &body).await;
        assert_eq!(
            reported,
            seeded[..5].to_vec(),
            "case {case:?}: expected the first five reports, got {reported:?}"
        );
        let token = first["NextToken"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            token, seeded[5],
            "case {case:?}: expected the token to name the node the next page starts at, \
             got {token:?}"
        );

        // The next page continues at the node the token names and ends the walk.
        let mut resumed = body.clone();
        resumed["NextToken"] = json!(token);
        let (second, reported) = read(&router, &case, &resumed).await;
        assert_eq!(
            reported,
            seeded[5..].to_vec(),
            "case {case:?}: expected the remaining reports, got {reported:?}"
        );
        assert!(
            second["NextToken"].is_null(),
            "case {case:?}: expected no token on the last page, got {:?}",
            second["NextToken"]
        );
    }

    // A page of a FILTERED read is a page of the reports that filter selected.
    let body = json!({
        "PatchGroup": GROUP,
        "Filters": [filter("InstalledCount", "GreaterThan", "4")],
    });
    let (_, reported) = read(&router, "filtered page", &body).await;
    assert_eq!(
        reported,
        seeded[4..].to_vec(),
        "case \"filtered page\": expected the three reports above the bound, got {reported:?}"
    );
}

#[tokio::test]
async fn a_next_token_this_read_never_handed_out_is_refused() {
    let (router, engine) = service();
    seed_mixed_reports(&engine);

    // Rows differ in how the token fails to be one this read hands out — the id of a
    // managed node (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`): an offset, a truncated id, an
    // id of the wrong length and an opaque cursor of another service's shape.
    let cases: &[&str] = &[
        "5",
        "i-",
        "i-0000",
        "i-000000011",
        "mi-0000000000000000",
        "AAEAAWgAAAA",
    ];

    for token in cases {
        let case = format!("NextToken={token}");
        let body = json!({ "PatchGroup": GROUP, "NextToken": token });
        let (status, parsed) = call(&router, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed:?}"
        );
        assert_eq!(
            error_code(&parsed),
            "InvalidNextToken",
            "case {case:?}: expected InvalidNextToken, got {parsed:?}"
        );
    }

    // A token shaped like a node id resumes the walk there, whether or not that node is
    // one of this group's: the walk is ordered by node id, so the page is the suffix.
    let suffix_cases: &[(&str, &[&str])] = &[
        (NODE_A, &[NODE_A, NODE_B, NODE_C]),
        (NODE_B, &[NODE_B, NODE_C]),
        ("i-00000005", &[NODE_C]),
        ("mi-99999999999999999", &[]),
    ];
    for (token, expected) in suffix_cases {
        let case = format!("NextToken={token}");
        let body = json!({ "PatchGroup": GROUP, "NextToken": token });
        let (_, reported) = read(&router, &case, &body).await;
        assert_eq!(
            reported,
            ids(expected),
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_read_reports_every_member_the_patching_operation_persisted_and_writes_nothing() {
    let (router, engine) = service();
    let (started, ended) = ran_at();

    // One report carrying a figure for EVERY member of `InstancePatchState`, so the case
    // pins the projection rather than the handful of members a filter looks at.
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
    report.owner_information = Some("owner-1".to_string());
    report.installed_count = 11;
    report.installed_other_count = 12;
    report.installed_pending_reboot_count = Some(13);
    report.installed_rejected_count = Some(14);
    report.missing_count = 15;
    report.failed_count = 16;
    report.unreported_not_applicable_count = Some(17);
    report.not_applicable_count = 18;
    report.available_security_update_count = Some(19);
    report.last_no_reboot_install_operation_time = Some(ended);
    report.reboot_option = Some("RebootIfNeeded".to_string());
    report.critical_non_compliant_count = Some(20);
    report.security_non_compliant_count = Some(21);
    report.other_non_compliant_count = Some(22);
    report_state(&engine, REGION, report);

    let (body, reported) = read(&router, "every member", &whole_group(GROUP)).await;
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
        ("OwnerInformation", "owner-1"),
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

    // A read reports; it must not change what a later caller sees.
    let before = stored_count(&engine);
    let (_, again) = read(&router, "read twice", &whole_group(GROUP)).await;
    assert_eq!(
        again,
        ids(&[NODE_A]),
        "case \"read twice\": expected the same report, got {again:?}"
    );
    assert_eq!(
        stored_count(&engine),
        before,
        "a read must leave the store as it found it"
    );
}

#[tokio::test]
async fn a_group_nothing_has_reported_for_holds_no_state() {
    let (router, engine) = service();

    // Nothing seeded: patch compliance is gathered on a node by the patching operation
    // Patch Manager runs there, so an account that has had none run holds no state and the
    // read reports none rather than inventing compliance.
    let (_, reported) = read(&router, "nothing reported", &whole_group(GROUP)).await;
    assert!(
        reported.is_empty(),
        "case \"nothing reported\": expected no state, got {reported:?}"
    );
    assert_eq!(
        stored_count(&engine),
        0,
        "a read of an empty store must write nothing"
    );
}
