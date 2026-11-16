// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for which accounts `BatchGetAccountStatus` will report a status for,
//! driven through `Enable` and `BatchGetAccountStatus` over the real `MultiServiceRouter` —
//! routing, restJson1 deserialization, modelled-constraint validation, the handler and
//! restJson1 serialization — so what is asserted here is the wire behaviour a client sees.
//!
//! The rule under test: an account-scoped read reports in `accounts` only the requested
//! accounts the calling account may read — its own account and the accounts of the
//! organization it administers — and reports every OTHER requested account per entry in
//! `failedAccounts`, with `errorCode` `ACCESS_DENIED` and the service's access message,
//! never a synthesized status. The request as a whole still succeeds.

use serde_json::{json, Value};

use crate::filters_and_enablement::{post_in, router};

/// `BatchGetAccountStatus`'s route.
/// Source: `smithy_query(query="operation", operation="BatchGetAccountStatus")` — `POST
/// /status/batch/get`.
const STATUS_URI: &str = "/status/batch/get";

/// `Enable`'s route, which is how the administrator brings an account into its organization
/// in this emulator.
/// Source: `smithy_query(query="operation", operation="Enable")` — `POST /enable`.
const ENABLE_URI: &str = "/enable";

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
