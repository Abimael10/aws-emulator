// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for which accounts this service's account-scoped batch reads will
//! report for, and for the free trial window one of them reports, driven through `Enable`,
//! `Disable`, `BatchGetAccountStatus` and `BatchGetFreeTrialInfo` over the real
//! `MultiServiceRouter` — routing, restJson1 deserialization, modelled-constraint
//! validation, the handler and restJson1 serialization — so what is asserted here is the
//! wire behaviour a client sees.
//!
//! The rules under test: an account-scoped read reports in `accounts` only the requested
//! accounts the calling account may read — its own account and the accounts of the
//! organization it administers — and reports every OTHER requested account per entry in
//! `failedAccounts`, with `ACCESS_DENIED` and the service's access message, never a
//! synthesized status. The request as a whole still succeeds. And: an account's free trial
//! of a scan type runs for the documented 15 days from the instant that scan type's
//! scanning was first switched on for the account, is reported ACTIVE while that window is
//! open and INACTIVE once it has closed, and does not exist at all for a scan type that was
//! never switched on.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_inspector2_inspector2::state::{
    self, FREE_TRIAL_DURATION, RESOURCE_SCAN_TYPES,
};

use crate::filters_and_enablement::{post_in, router, router_over};

/// `BatchGetAccountStatus`'s route.
/// Source: `smithy_query(query="operation", operation="BatchGetAccountStatus")` — `POST
/// /status/batch/get`.
const STATUS_URI: &str = "/status/batch/get";

/// `Enable`'s route, which is how the administrator brings an account into its organization
/// in this emulator.
/// Source: `smithy_query(query="operation", operation="Enable")` — `POST /enable`.
const ENABLE_URI: &str = "/enable";

/// `Disable`'s route, which is how a scan type is switched back off.
/// Source: `smithy_query(query="operation", operation="Disable")` — `POST /disable`.
const DISABLE_URI: &str = "/disable";

/// `BatchGetFreeTrialInfo`'s route.
/// Source: `smithy_query(query="operation", operation="BatchGetFreeTrialInfo")` — `POST
/// /freetrialinfo/batchget`.
const FREE_TRIAL_URI: &str = "/freetrialinfo/batchget";

/// How many seconds one scan type's Amazon Inspector free trial runs for: Amazon Inspector
/// documents a 15-day free trial of each scan type.
const FREE_TRIAL_SECONDS: f64 = 15.0 * 24.0 * 60.0 * 60.0;

/// How far two instants rendered as epoch-second numbers may differ from the span between
/// them and still be the same span: a timestamp is carried as a double, whose resolution
/// around the present epoch is far finer than this, so a millisecond of slack compares the
/// span rather than the rounding.
const INSTANT_TOLERANCE_SECONDS: f64 = 1e-3;

/// The `FreeTrialStatus` token of a trial window that is still open.
/// Source: `smithy_query(query="shape", shape="FreeTrialStatus")` — `ACTIVE | INACTIVE`.
const FREE_TRIAL_ACTIVE: &str = "ACTIVE";

/// The `FreeTrialStatus` token of a trial window that has closed.
/// Source: `smithy_query(query="shape", shape="FreeTrialStatus")`.
const FREE_TRIAL_INACTIVE: &str = "INACTIVE";

/// The single account id every generated emulator presents
/// (`engine::emulator_account_id`), which is the calling account of every request here.
const CALLER: &str = "000000000000";

/// The `ErrorCode` token a per-account entry carries for an account the caller may not
/// read.
/// Source: `smithy_query(query="shape", shape="ErrorCode")`.
const ACCESS_DENIED: &str = "ACCESS_DENIED";

/// How the service words that entry's `errorMessage`: recorded behaviour, whose text is
/// fixed — the entry's `accountId` is the only part that varies with the request.
const NO_ACCESS_MESSAGE: &str = "Invoking account does not have access to this account";

/// One row of the table.
#[derive(Debug)]
struct Case {
    /// What the row is about, named in every assertion.
    name: String,
    /// The accounts `Enable` records before the read, which is what makes them members of
    /// the organization the caller administers.
    enabled: Vec<String>,
    /// The `accountIds` the read asks about — `None` for a request that leaves the optional
    /// member out entirely.
    requested: Option<Vec<String>>,
    /// The ids expected in `accounts`, in the order they are expected.
    in_accounts: Vec<String>,
    /// The ids expected in `failedAccounts`, in the order they are expected.
    denied: Vec<String>,
}

/// A 12-digit account id this store has no record of, built from `seed` so each row can use
/// ids of its own.
/// Source: `smithy_query(query="shape", shape="AccountId")` — `^\d{12}$`.
fn stranger(seed: usize) -> String {
    format!("9{seed:011}")
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// Send the read and assert the row's expectation: HTTP 200, exactly the member ids in
/// `accounts`, and exactly the other ids in `failedAccounts`, each carrying the access
/// refusal and NO status of any kind.
async fn assert_row(case: &Case) {
    let router = router();
    if !case.enabled.is_empty() {
        let (status, _, enabled) = post_in(
            &router,
            "us-east-1",
            ENABLE_URI,
            &json!({ "resourceTypes": ["EC2"], "accountIds": case.enabled }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {}: Enable: expected 200, got {status} / {enabled}",
            case.name
        );
    }

    let body = match &case.requested {
        Some(requested) => json!({ "accountIds": requested }),
        None => json!({}),
    };
    let (status, error_type, response) = post_in(&router, "us-east-1", STATUS_URI, &body).await;
    assert_eq!(
        status, 200,
        "case {}: expected 200, got {status} / {response} (errortype {error_type:?})",
        case.name
    );

    let reported: Vec<String> = response["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("case {}: expected an accounts list, got {response}", case.name))
        .iter()
        .map(|account| {
            account["accountId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {}: expected each account its id, got {response}", case.name)
                })
                .to_string()
        })
        .collect();
    assert_eq!(
        reported, case.in_accounts,
        "case {}: expected exactly the readable accounts reported, got {response}",
        case.name
    );
    for account in &reported {
        let expected = if case.enabled.contains(account) {
            "ENABLED"
        } else {
            "DISABLED"
        };
        let entry = response["accounts"]
            .as_array()
            .expect("an accounts list")
            .iter()
            .find(|candidate| candidate["accountId"] == json!(account))
            .expect("the account just read back");
        assert_eq!(
            entry["state"]["status"], json!(expected),
            "case {} / {account}: expected {expected}, got {response}",
            case.name
        );
    }

    if case.denied.is_empty() {
        assert!(
            response["failedAccounts"].is_null(),
            "case {}: nothing failed, so failedAccounts must be absent, got {response}",
            case.name
        );
        return;
    }

    let failed = response["failedAccounts"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {}: expected a failedAccounts list, got {response}",
                case.name
            )
        })
        .clone();
    let failed_ids: Vec<String> = failed
        .iter()
        .map(|entry| {
            entry["accountId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {}: expected each entry its id, got {response}", case.name)
                })
                .to_string()
        })
        .collect();
    assert_eq!(
        failed_ids, case.denied,
        "case {}: expected exactly the unreadable accounts reported per entry, got {response}",
        case.name
    );
    for entry in &failed {
        let account = entry["accountId"].as_str().unwrap_or_default();
        assert_eq!(
            entry["errorCode"], json!(ACCESS_DENIED),
            "case {} / {account}: expected errorCode {ACCESS_DENIED}, got {entry}",
            case.name
        );
        assert_eq!(
            entry["errorMessage"], json!(NO_ACCESS_MESSAGE),
            "case {} / {account}: unexpected errorMessage, got {entry}",
            case.name
        );
        for synthesized in ["status", "resourceStatus"] {
            assert_eq!(
                entry.get(synthesized).cloned().unwrap_or(Value::Null),
                Value::Null,
                "case {} / {account}: an account the caller may not read must carry no \
                 {synthesized}, got {entry}",
                case.name
            );
        }
    }
}

/// Only the accounts the caller may read are reported with a status; every other requested
/// account is reported per entry as access-denied.
///
/// The rows differ along WHICH ids are readable and how many are not, along where an
/// unreadable id sits in the request, along the form of `accountIds` (a list, an explicitly
/// empty list, the member left out) and along the request's size, up to the modelled list
/// bound.
/// Source: `smithy_query(query="shape", shape="AccountIdSet")` — length 0..100,
/// `uniqueItems`.
#[tokio::test]
async fn only_accounts_the_caller_administers_are_reported_with_a_status() {
    // The caller's own account, named alone: the positive control.
    let mut cases = vec![
        Case {
            name: "the caller alone".to_string(),
            enabled: Vec::new(),
            requested: Some(ids(&[CALLER])),
            in_accounts: ids(&[CALLER]),
            denied: Vec::new(),
        },
        Case {
            name: "no accountIds member at all".to_string(),
            enabled: Vec::new(),
            requested: None,
            in_accounts: ids(&[CALLER]),
            denied: Vec::new(),
        },
        Case {
            name: "an explicitly empty accountIds".to_string(),
            enabled: Vec::new(),
            requested: Some(Vec::new()),
            in_accounts: ids(&[CALLER]),
            denied: Vec::new(),
        },
        Case {
            name: "the caller and one account it does not administer".to_string(),
            enabled: Vec::new(),
            requested: Some(vec![CALLER.to_string(), stranger(1)]),
            in_accounts: ids(&[CALLER]),
            denied: vec![stranger(1)],
        },
        Case {
            name: "the unreadable id first".to_string(),
            enabled: Vec::new(),
            requested: Some(vec![stranger(2), CALLER.to_string()]),
            in_accounts: ids(&[CALLER]),
            denied: vec![stranger(2)],
        },
        Case {
            name: "only accounts the caller does not administer".to_string(),
            enabled: Vec::new(),
            requested: Some(vec![stranger(3), stranger(4)]),
            in_accounts: Vec::new(),
            denied: vec![stranger(3), stranger(4)],
        },
        Case {
            name: "a member the administrator enabled, beside a stranger".to_string(),
            enabled: ids(&["012345678901"]),
            requested: Some(vec![
                CALLER.to_string(),
                "012345678901".to_string(),
                stranger(5),
            ]),
            in_accounts: ids(&[CALLER, "012345678901"]),
            denied: vec![stranger(5)],
        },
        Case {
            name: "members and strangers interleaved".to_string(),
            enabled: ids(&["012345678901", "109876543210"]),
            requested: Some(vec![
                stranger(6),
                "012345678901".to_string(),
                stranger(7),
                "109876543210".to_string(),
                CALLER.to_string(),
            ]),
            in_accounts: ids(&["012345678901", "109876543210", CALLER]),
            denied: vec![stranger(6), stranger(7)],
        },
    ];

    // A request at the modelled list bound: the caller plus 99 accounts it does not
    // administer.
    let strangers: Vec<String> = (10..109).map(stranger).collect();
    let mut requested = vec![CALLER.to_string()];
    requested.extend(strangers.iter().cloned());
    cases.push(Case {
        name: "a hundred ids, the modelled list bound".to_string(),
        enabled: Vec::new(),
        requested: Some(requested),
        in_accounts: ids(&[CALLER]),
        denied: strangers,
    });

    for case in &cases {
        assert_row(case).await;
    }
}

/// The request is bounded by the MODEL, and the boundary is where that bound is enforced: a
/// list one past `AccountIdSet`'s published maximum is refused before any per-account
/// decision is taken, and so is a member outside `AccountId`'s value space — so the answer
/// can hold at most a hundred entries however many ids a caller sends, and a malformed id is
/// a bad request rather than an account the caller may not read.
///
/// The rows differ along WHICH part of the input is outside the model: the list's length, and
/// the member's length and character set (too short, too long, non-numeric, empty). The
/// in-bound side of the same rule — a request of exactly a hundred ids — is a row of the
/// table above.
/// Source: `smithy_query(query="shape", shape="AccountIdSet,AccountId")` — list length
/// 0..100 with `uniqueItems`, member length 12 with pattern `^\d{12}$`.
#[tokio::test]
async fn a_request_outside_the_modelled_account_id_space_is_refused() {
    let one_past_the_bound: Vec<String> = (200..301).map(stranger).collect();
    let cases: Vec<(String, Vec<String>)> = vec![
        (
            "a hundred and one ids, one past the list bound".to_string(),
            one_past_the_bound,
        ),
        ("an eleven-digit id".to_string(), ids(&["12345678901"])),
        ("a thirteen-digit id".to_string(), ids(&["1234567890123"])),
        ("a non-numeric id".to_string(), ids(&["not-an-account"])),
        ("an empty id".to_string(), ids(&[""])),
    ];

    for (case, requested) in cases {
        let router = router();
        let (status, error_type, response) = post_in(
            &router,
            "us-east-1",
            STATUS_URI,
            &json!({ "accountIds": requested }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} / {response}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {error_type:?} / {response}"
        );
        assert!(
            response["accounts"].is_null() && response["failedAccounts"].is_null(),
            "case {case}: a refused request must report no account at all, got {response}"
        );
    }
}

/// The decision comes from recorded state, not from the id: an account the caller may not
/// read is reported per entry, and the SAME id is reported with a status once `Enable` has
/// brought it into the organization the caller administers.
#[tokio::test]
async fn an_account_becomes_readable_once_it_is_recorded() {
    let joining = stranger(42);
    let router = router();

    let (status, _, before) = post_in(
        &router,
        "us-east-1",
        STATUS_URI,
        &json!({ "accountIds": [CALLER, joining] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "before Enable: expected 200, got {status} / {before}"
    );
    assert_eq!(
        before["accounts"].as_array().map(Vec::len),
        Some(1),
        "before Enable: only the caller is readable, got {before}"
    );
    assert_eq!(
        before["failedAccounts"][0]["accountId"], json!(joining),
        "before Enable: expected {joining} reported per entry, got {before}"
    );
    assert_eq!(
        before["failedAccounts"][0]["errorCode"], json!(ACCESS_DENIED),
        "before Enable: expected errorCode {ACCESS_DENIED}, got {before}"
    );

    let (status, _, enabled) = post_in(
        &router,
        "us-east-1",
        ENABLE_URI,
        &json!({ "resourceTypes": ["ECR"], "accountIds": [joining] }),
    )
    .await;
    assert_eq!(status, 200, "Enable: expected 200, got {status} / {enabled}");

    let (status, _, after) = post_in(
        &router,
        "us-east-1",
        STATUS_URI,
        &json!({ "accountIds": [CALLER, joining] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "after Enable: expected 200, got {status} / {after}"
    );
    assert!(
        after["failedAccounts"].is_null(),
        "after Enable: nothing fails any more, got {after}"
    );
    assert_eq!(
        after["accounts"][1]["accountId"], json!(joining),
        "after Enable: expected {joining} reported in request order, got {after}"
    );
    assert_eq!(
        after["accounts"][1]["resourceState"]["ecr"]["status"], json!("ENABLED"),
        "after Enable: expected the recorded enablement reported, got {after}"
    );
}

/// Switch `resource_types` on for `account_ids` — the calling account when the list is
/// empty — and fail the test if `Enable` itself did not answer.
async fn enable_scan_types(
    router: &smithy_server_runtime::MultiServiceRouter,
    account_ids: &[String],
    resource_types: &[&str],
) {
    let mut request = json!({ "resourceTypes": resource_types });
    if !account_ids.is_empty() {
        request["accountIds"] = json!(account_ids);
    }
    let (status, error_type, answer) = post_in(router, "us-east-1", ENABLE_URI, &request).await;
    assert_eq!(
        status, 200,
        "Enable {account_ids:?} / {resource_types:?}: expected 200, got {status} / {answer} \
         ({error_type:?})"
    );
}

/// Switch `resource_types` back off for the calling account, and fail the test if `Disable`
/// itself did not answer.
async fn disable_scan_types(
    router: &smithy_server_runtime::MultiServiceRouter,
    resource_types: &[&str],
) {
    let request = json!({ "resourceTypes": resource_types });
    let (status, error_type, answer) = post_in(router, "us-east-1", DISABLE_URI, &request).await;
    assert_eq!(
        status, 200,
        "Disable {resource_types:?}: expected 200, got {status} / {answer} ({error_type:?})"
    );
}

/// A `BatchGetFreeTrialInfo` call asking about `account_ids`, answered with 200.
async fn free_trial_info(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    account_ids: &[String],
) -> Value {
    let request = json!({ "accountIds": account_ids });
    let (status, error_type, answer) = post_in(router, "us-east-1", FREE_TRIAL_URI, &request).await;
    assert_eq!(
        status, 200,
        "case {case}: expected 200, got {status} / {answer} ({error_type:?})"
    );
    answer
}

/// The `type` of every free trial entry one account's report carries, in the order it lists
/// them.
fn trial_types_in(account: &Value) -> Vec<String> {
    account["freeTrialInfo"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["type"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One account's free trial entry for `scan_type`, or `Value::Null` when it reports none.
fn trial_entry(account: &Value, scan_type: &str) -> Value {
    account["freeTrialInfo"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["type"] == json!(scan_type))
        })
        .cloned()
        .unwrap_or(Value::Null)
}

/// Every free trial window a report carries spans exactly the documented trial length, is
/// reported ACTIVE while it is still open, and names the cloud provider whose resources the
/// scan covers.
fn assert_entries_are_open_fifteen_day_windows(case: &str, account: &Value) {
    for entry in account["freeTrialInfo"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let scan_type = entry["type"].as_str().unwrap_or_default().to_string();
        let start = entry["start"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case} / {scan_type}: no start instant in {entry}"));
        let end = entry["end"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case} / {scan_type}: no end instant in {entry}"));
        assert!(
            (end - start - FREE_TRIAL_SECONDS).abs() < INSTANT_TOLERANCE_SECONDS,
            "case {case} / {scan_type}: the window must span the documented trial length, got \
             {entry}"
        );
        assert_eq!(
            entry["status"], json!(FREE_TRIAL_ACTIVE),
            "case {case} / {scan_type}: a window that is still open must be reported {FREE_TRIAL_ACTIVE}, \
             got {entry}"
        );
        assert_eq!(
            entry["cloudProvider"], json!("AWS"),
            "case {case} / {scan_type}: an Amazon Web Services scan's trial covers Amazon Web \
             Services resources, got {entry}"
        );
    }
}

/// A free trial exists for exactly the scan types an account has switched on, each window
/// spanning the documented trial length from that switch-on and reported ACTIVE while it is
/// open — so an account that has switched nothing on reports no window at all.
///
/// The rows differ along WHICH and HOW MANY scan types are on, covering every modelled
/// `ResourceScanType` including the ones the issue never cited.
#[tokio::test]
async fn a_free_trial_window_is_reported_for_each_scan_type_switched_on() {
    let cases: [&[&str]; 8] = [
        &["EC2"],
        &["ECR"],
        &["LAMBDA"],
        &["LAMBDA_CODE"],
        &["CODE_REPOSITORY"],
        &["EC2", "ECR"],
        &["LAMBDA", "LAMBDA_CODE", "CODE_REPOSITORY"],
        &RESOURCE_SCAN_TYPES,
    ];

    for enabled in cases {
        let case = format!("{enabled:?}");
        let router = router();

        // Before `Enable`, the calling account has switched nothing on, so no trial of its
        // has begun: it is reported, with no window.
        let before = free_trial_info(&router, &case, &ids(&[CALLER])).await;
        assert_eq!(
            before["accounts"][0]["accountId"], json!(CALLER),
            "case {case}: the requested account must be reported, got {before}"
        );
        assert_eq!(
            before["accounts"][0]["freeTrialInfo"],
            json!([]),
            "case {case}: an account with no scan switched on has no trial window, got {before}"
        );
        assert_eq!(
            before["failedAccounts"],
            json!([]),
            "case {case}: the caller may read its own account, so nothing fails, got {before}"
        );

        enable_scan_types(&router, &[], enabled).await;

        let after = free_trial_info(&router, &case, &ids(&[CALLER])).await;
        let account = after["accounts"][0].clone();
        let expected: Vec<String> = RESOURCE_SCAN_TYPES
            .iter()
            .filter(|scan_type| enabled.contains(*scan_type))
            .map(|scan_type| (*scan_type).to_string())
            .collect();
        assert_eq!(
            trial_types_in(&account),
            expected,
            "case {case}: exactly the scan types switched on must report a window, in model \
             order, got {after}"
        );
        assert_entries_are_open_fifteen_day_windows(&case, &account);

        // The read reports state rather than changing it: a second call answers the same
        // windows.
        let again = free_trial_info(&router, &case, &ids(&[CALLER])).await;
        assert_eq!(
            again["accounts"][0], account,
            "case {case}: reading the free trial must not change what a later caller sees, got \
             {again}"
        );
    }
}

/// A trial window is a span of the account's calendar, not of its scanning: once begun it
/// keeps its instants while the scan type is switched off, and switching that scan type on
/// again neither restarts it nor hands out a second one. A scan type switched on LATER gets
/// its own window, which starts no earlier than the first one's.
#[tokio::test]
async fn a_begun_trial_window_keeps_its_instants_across_disable_and_re_enable() {
    let router = router();
    enable_scan_types(&router, &[], &["EC2"]).await;

    let first = free_trial_info(&router, "after the first Enable", &ids(&[CALLER])).await;
    let begun = trial_entry(&first["accounts"][0], "EC2");
    assert!(
        begun["start"].is_number(),
        "case after the first Enable: EC2's trial must have begun, got {first}"
    );

    // Rows: the window after switching EC2 off, after switching it on again, and after a
    // second `Enable` of the same scan type.
    disable_scan_types(&router, &["EC2"]).await;
    let while_off = free_trial_info(&router, "while EC2 is off", &ids(&[CALLER])).await;
    assert_eq!(
        trial_entry(&while_off["accounts"][0], "EC2"),
        begun,
        "case while EC2 is off: the trial keeps running, with the same instants, got {while_off}"
    );

    enable_scan_types(&router, &[], &["EC2"]).await;
    let re_enabled = free_trial_info(&router, "after re-Enable", &ids(&[CALLER])).await;
    assert_eq!(
        trial_entry(&re_enabled["accounts"][0], "EC2"),
        begun,
        "case after re-Enable: the trial must not start over, got {re_enabled}"
    );
    assert_eq!(
        trial_types_in(&re_enabled["accounts"][0]),
        vec!["EC2".to_string()],
        "case after re-Enable: no second window may be handed out, got {re_enabled}"
    );

    // A scan type switched on only now gets a window of its own, which begins no earlier
    // than the window that was already running.
    enable_scan_types(&router, &[], &["LAMBDA"]).await;
    let both = free_trial_info(&router, "after Enable LAMBDA", &ids(&[CALLER])).await;
    assert_eq!(
        trial_types_in(&both["accounts"][0]),
        vec!["EC2".to_string(), "LAMBDA".to_string()],
        "case after Enable LAMBDA: both scan types switched on must report a window, in model \
         order, got {both}"
    );
    assert_eq!(
        trial_entry(&both["accounts"][0], "EC2"),
        begun,
        "case after Enable LAMBDA: the window already running must not move, got {both}"
    );
    let lambda_start = trial_entry(&both["accounts"][0], "LAMBDA")["start"]
        .as_f64()
        .unwrap_or_else(|| panic!("case after Enable LAMBDA: no start instant in {both}"));
    let ec2_start = begun["start"]
        .as_f64()
        .unwrap_or_else(|| panic!("case after Enable LAMBDA: no EC2 start instant in {both}"));
    assert!(
        lambda_start >= ec2_start,
        "case after Enable LAMBDA: a trial begun later cannot start earlier (EC2 {ec2_start}, \
         LAMBDA {lambda_start}), got {both}"
    );
    assert_entries_are_open_fifteen_day_windows("after Enable LAMBDA", &both["accounts"][0]);
}

/// One row of the free trial access table.
#[derive(Debug)]
struct TrialCase {
    /// What the row is about, named in every assertion.
    name: String,
    /// The `accountIds` the read asks about.
    requested: Vec<String>,
    /// The ids expected in `accounts`, in the order they are expected.
    readable: Vec<String>,
    /// The ids expected in `failedAccounts`, in the order they are expected.
    denied: Vec<String>,
}

/// The free trial read is account-scoped like the status read: only the accounts the caller
/// may read report a window, every other requested account is reported per entry with
/// `ACCESS_DENIED` and no window, and an id named twice names one account.
///
/// The rows differ along WHICH ids are readable, how many are not, where an unreadable id
/// sits in the request, whether an id repeats, and the request's size up to the modelled
/// list bound.
/// Source: `smithy_query(query="shape", shape="MeteringAccountIdList")` — length 1..100.
#[tokio::test]
async fn only_accounts_the_caller_administers_report_a_free_trial() {
    let member = "012345678901".to_string();
    let mut cases: Vec<TrialCase> = vec![
        TrialCase {
            name: "the caller alone".to_string(),
            requested: ids(&[CALLER]),
            readable: ids(&[CALLER]),
            denied: Vec::new(),
        },
        TrialCase {
            name: "the caller and one account it does not administer".to_string(),
            requested: vec![CALLER.to_string(), stranger(21)],
            readable: ids(&[CALLER]),
            denied: vec![stranger(21)],
        },
        TrialCase {
            name: "the unreadable id first".to_string(),
            requested: vec![stranger(22), CALLER.to_string()],
            readable: ids(&[CALLER]),
            denied: vec![stranger(22)],
        },
        TrialCase {
            name: "only accounts the caller does not administer".to_string(),
            requested: vec![stranger(23), stranger(24)],
            readable: Vec::new(),
            denied: vec![stranger(23), stranger(24)],
        },
        TrialCase {
            name: "an id named twice".to_string(),
            requested: vec![CALLER.to_string(), CALLER.to_string()],
            readable: ids(&[CALLER]),
            denied: Vec::new(),
        },
        TrialCase {
            name: "a member of the organization, beside a stranger".to_string(),
            requested: vec![member.clone(), stranger(25), CALLER.to_string()],
            readable: vec![member.clone(), CALLER.to_string()],
            denied: vec![stranger(25)],
        },
    ];

    // A request at the modelled list bound: the caller plus 99 accounts it does not
    // administer.
    let strangers: Vec<String> = (30..129).map(stranger).collect();
    let mut at_the_bound = vec![CALLER.to_string()];
    at_the_bound.extend(strangers.iter().cloned());
    cases.push(TrialCase {
        name: "a hundred ids, the modelled list bound".to_string(),
        requested: at_the_bound,
        readable: ids(&[CALLER]),
        denied: strangers,
    });

    for TrialCase {
        name: case,
        requested,
        readable,
        denied,
    } in cases
    {
        let router = router();
        // The member account is brought into the organization, and the caller switches a
        // scan type on, so a readable account really has a window to report.
        enable_scan_types(&router, std::slice::from_ref(&member), &["ECR"]).await;
        enable_scan_types(&router, &[], &["EC2"]).await;

        let answer = free_trial_info(&router, &case, &requested).await;
        let reported: Vec<String> = answer["accounts"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: expected an accounts list, got {answer}"))
            .iter()
            .map(|account| {
                account["accountId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            reported, readable,
            "case {case}: exactly the readable accounts must be reported, in request order, got \
             {answer}"
        );

        let failed = answer["failedAccounts"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {case}: expected a failedAccounts list, got {answer}"));
        let failed_ids: Vec<String> = failed
            .iter()
            .map(|entry| entry["accountId"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            failed_ids, denied,
            "case {case}: exactly the unreadable accounts must be reported per entry, got {answer}"
        );
        for entry in &failed {
            let account = entry["accountId"].as_str().unwrap_or_default();
            assert_eq!(
                entry["code"], json!(ACCESS_DENIED),
                "case {case} / {account}: expected code {ACCESS_DENIED}, got {entry}"
            );
            assert_eq!(
                entry["message"], json!(NO_ACCESS_MESSAGE),
                "case {case} / {account}: unexpected message, got {entry}"
            );
            assert_eq!(
                entry.get("freeTrialInfo").cloned().unwrap_or(Value::Null),
                Value::Null,
                "case {case} / {account}: an account the caller may not read must carry no \
                 window, got {entry}"
            );
        }

        // Each readable account reports ITS OWN scan types' windows rather than the
        // request's or another account's.
        for (index, account_id) in readable.iter().enumerate() {
            let expected = if *account_id == member {
                vec!["ECR".to_string()]
            } else {
                vec!["EC2".to_string()]
            };
            assert_eq!(
                trial_types_in(&answer["accounts"][index]),
                expected,
                "case {case} / {account_id}: the report must carry that account's own windows, \
                 got {answer}"
            );
            assert_entries_are_open_fifteen_day_windows(&case, &answer["accounts"][index]);
        }
    }
}

/// The free trial request is bounded by the MODEL, and the boundary is where that bound is
/// enforced: an empty list, a list one past the published maximum and an id outside
/// `MeteringAccountId`'s value space are all refused before any per-account decision is
/// taken, so the answer can hold at most a hundred reports and a malformed id is a bad
/// request rather than an account the caller may not read.
///
/// Source: `smithy_query(query="shape", shape="MeteringAccountIdList,MeteringAccountId")` —
/// list length 1..100, member pattern `[0-9]{12}`.
#[tokio::test]
async fn a_free_trial_request_outside_the_modelled_account_id_space_is_refused() {
    let one_past_the_bound: Vec<String> = (300..401).map(stranger).collect();
    let cases: Vec<(String, Value)> = vec![
        ("an empty accountIds".to_string(), json!({ "accountIds": [] })),
        (
            "no accountIds member at all".to_string(),
            json!({}),
        ),
        (
            "a hundred and one ids, one past the list bound".to_string(),
            json!({ "accountIds": one_past_the_bound }),
        ),
        (
            "an eleven-digit id".to_string(),
            json!({ "accountIds": ["12345678901"] }),
        ),
        (
            "a non-numeric id".to_string(),
            json!({ "accountIds": ["not-an-account"] }),
        ),
    ];

    for (case, request) in cases {
        let router = router();
        let (status, error_type, response) =
            post_in(&router, "us-east-1", FREE_TRIAL_URI, &request).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} / {response}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {error_type:?} / {response}"
        );
        assert!(
            response["accounts"].is_null() && response["failedAccounts"].is_null(),
            "case {case}: a refused request must report no account at all, got {response}"
        );
    }
}

/// The reported `status` follows WHERE the read falls in the window, not how the trial was
/// recorded: a window still open is ACTIVE and one that has closed is INACTIVE, while the
/// window's own instants stay exactly as they were recorded.
///
/// The rows are trials of different ages, seeded into the store the handlers read (nothing
/// waits for a 15-day window to close, and no row depends on how fast the host runs): fresh,
/// one hour into the window, an hour short of its end, an hour past its end, and long past
/// it.
#[tokio::test]
async fn a_closed_trial_window_is_reported_inactive() {
    let hour = Duration::from_secs(60 * 60);
    let cases: [(&str, Duration, &str); 5] = [
        ("a fresh trial", Duration::ZERO, FREE_TRIAL_ACTIVE),
        ("an hour into the window", hour, FREE_TRIAL_ACTIVE),
        (
            "an hour short of the end",
            FREE_TRIAL_DURATION - hour,
            FREE_TRIAL_ACTIVE,
        ),
        (
            "an hour past the end",
            FREE_TRIAL_DURATION + hour,
            FREE_TRIAL_INACTIVE,
        ),
        (
            "long past the end",
            FREE_TRIAL_DURATION * 10,
            FREE_TRIAL_INACTIVE,
        ),
    ];

    for (case, age, expected_status) in cases {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_over(&engine);
        enable_scan_types(&router, &[], &["EC2"]).await;

        // The trial began `age` ago. Only `Enable` starts a trial, and the window it
        // started is this account's record, so the age is set where that record lives
        // rather than by waiting for the clock.
        let began = SystemTime::now() - age;
        state::service_state(&*engine)
            .expect("the inspector2 store")
            .lock()
            .accounts
            .get_mut(CALLER)
            .expect("the account Enable just recorded")
            .trial_starts
            .insert("EC2".to_string(), began);

        let answer = free_trial_info(&router, case, &ids(&[CALLER])).await;
        let entry = trial_entry(&answer["accounts"][0], "EC2");
        assert_eq!(
            entry["status"], json!(expected_status),
            "case {case}: expected {expected_status}, got {answer}"
        );

        let start = entry["start"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: no start instant in {answer}"));
        let end = entry["end"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: no end instant in {answer}"));
        let recorded = began
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("an instant after the epoch")
            .as_secs_f64();
        assert!(
            (start - recorded).abs() < INSTANT_TOLERANCE_SECONDS,
            "case {case}: the window must begin where it was recorded ({recorded}), got {start}"
        );
        assert!(
            (end - start - FREE_TRIAL_SECONDS).abs() < INSTANT_TOLERANCE_SECONDS,
            "case {case}: a closed window keeps the documented length, got {answer}"
        );
    }
}
