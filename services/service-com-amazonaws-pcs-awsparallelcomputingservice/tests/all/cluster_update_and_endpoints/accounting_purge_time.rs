// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH accounting purge times a PCS cluster keeps.
//!
//! RULE under test: a value a member's own documentation EXCLUDES is refused with the
//! modelled `ValidationException`, even when the member's `@range` admits it — a `@range`
//! states an interval and cannot leave a hole inside it, so an exclusion inside the interval
//! is the service's to state. `defaultPurgeTimeInDays` is modelled
//! `@range(min: -1, max: 10000)` and its documentation adds "`0` isn't a valid value"
//! (`smithy_query(query="shape",shape="AccountingRequest")`, worded identically on
//! `UpdateAccountingRequest`), so every path that WRITES the member — `CreateCluster` and
//! `UpdateCluster` — refuses that one value and keeps every other the range admits.
//!
//! Driven over the awsJson1_0 wire through the same router the emulator serves from, on the
//! harness [`crate::cluster_lifecycle`] builds. Nothing here waits on the wall clock: a
//! transition this service owes is materialised before the next request is answered.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::{
    EXCLUDED_PURGE_TIME_IN_DAYS, PURGE_TIME_MEMBER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{call, emulator};

/// Purge times the member admits: (case, the value).
///
/// Source: `AccountingRequest$defaultPurgeTimeInDays`, modelled `@range(min: -1, max: 10000)`
/// with "The default value for `defaultPurgeTimeInDays` is `-1`. A value of `-1` means there
/// is no purge time and records persist as long as the cluster exists"
/// (`smithy_query(query="shape",shape="AccountingRequest")`). The rows are both PUBLISHED
/// bounds of that range, the smallest and largest number of days inside it, and a value the
/// rest of this crate's tests do not send.
const ADMITTED: &[(&str, i32)] = &[
    ("published-min-no-purge", -1),
    ("one-day", 1),
    ("a-week", 7),
    ("a-year", 365),
    ("published-max", 10_000),
];

/// Purge times OUTSIDE the member's published range: one past each bound, refused by the
/// `@range` itself rather than by the documented exclusion.
const OUTSIDE_THE_RANGE: &[(&str, i32)] = &[
    ("one-below-the-published-min", -2),
    ("one-above-the-published-max", 10_001),
];

/// The `slurmConfiguration` a request carries to set the accounting purge time, with
/// `mode` — `@required` on the create path — always named.
fn accounting(purge_time: Option<i32>) -> Value {
    let mut body = json!({"accounting": {"mode": "STANDARD"}});
    if let Some(purge_time) = purge_time {
        body["accounting"]["defaultPurgeTimeInDays"] = json!(purge_time);
    }
    body
}

/// A `CreateCluster` request carrying every `@required` member, plus the accounting
/// configuration under test.
fn create_cluster(name: &str, purge_time: Option<i32>) -> Value {
    json!({
        "clusterName": name,
        "scheduler": {"type": "SLURM", "version": "24.11"},
        "size": "SMALL",
        "networking": {
            "subnetIds": ["subnet-0123456789abcdef0"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
        },
        "slurmConfiguration": accounting(purge_time),
    })
}

/// The purge time `GetCluster` reports for `identifier`, and the cluster's status, once the
/// transitions the service owes have been materialised by this very read.
async fn stored_purge_time(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> (Value, Value) {
    let (status, body) = call(
        router,
        "GetCluster",
        &json!({"clusterIdentifier": identifier}),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: GetCluster got {body}");
    (
        body["cluster"]["slurmConfiguration"]["accounting"]["defaultPurgeTimeInDays"].clone(),
        body["cluster"]["status"].clone(),
    )
}

/// The one sentence a value the member's documentation excludes is refused with, and the
/// structural detail beside it.
fn assert_excluded_value_refusal(case: &str, status: u16, refused: &Value) {
    let constraint = format!("Member must not have value equal to {EXCLUDED_PURGE_TIME_IN_DAYS}");
    let expected = format!(
        "1 validation error(s) detected: Value at '{PURGE_TIME_MEMBER}' failed to satisfy \
         constraint: {constraint}"
    );
    assert_eq!(
        (status, refused["__type"].as_str()),
        (CLIENT_ERROR_STATUS, Some("ValidationException")),
        "case {case:?}: expected the client-error status with ValidationException, got {status} \
         with {refused}"
    );
    assert_eq!(
        refused["message"].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected {expected:?}, got {refused}"
    );
    assert_eq!(
        refused["reason"].as_str(),
        Some("fieldValidationFailed"),
        "case {case:?}: expected the field-validation reason, got {refused}"
    );
    assert_eq!(
        refused["fieldList"],
        json!([{"name": PURGE_TIME_MEMBER, "message": constraint}]),
        "case {case:?}: expected one fieldList entry naming {PURGE_TIME_MEMBER:?}, got {refused}"
    );
}

/// The cluster names `ListClusters` reports.
async fn cluster_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = call(router, "ListClusters", &json!({"maxResults": 100})).await;
    assert_eq!(status, 200, "ListClusters got {listed}");
    listed["clusters"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|cluster| cluster["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn create_cluster_keeps_every_purge_time_the_member_admits() {
    let router = emulator();

    for (case, purge_time) in ADMITTED {
        let name = format!("purge-create-{case}");
        let (status, created) = call(
            &router,
            "CreateCluster",
            &create_cluster(&name, Some(*purge_time)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a purge time of {purge_time} to be served, got {status} \
             with {created}"
        );
        assert_eq!(
            created["cluster"]["slurmConfiguration"]["accounting"]["defaultPurgeTimeInDays"],
            json!(*purge_time),
            "case {case:?}: expected the create to answer {purge_time}, got {created}"
        );

        let (stored, cluster_status) = stored_purge_time(&router, case, &name).await;
        assert_eq!(
            stored,
            json!(*purge_time),
            "case {case:?}: expected {purge_time} to persist, got {stored}"
        );
        assert_eq!(
            cluster_status,
            json!("ACTIVE"),
            "case {case:?}: expected the cluster to settle, got {cluster_status}"
        );
    }
}

#[tokio::test]
async fn create_cluster_refuses_the_purge_time_the_member_excludes() {
    let router = emulator();

    let (status, refused) = call(
        &router,
        "CreateCluster",
        &create_cluster("purge-create-excluded", Some(EXCLUDED_PURGE_TIME_IN_DAYS)),
    )
    .await;
    assert_excluded_value_refusal("create with the excluded purge time", status, &refused);

    // Nothing was minted: the refused create left no cluster behind.
    let names = cluster_names(&router).await;
    assert!(
        names.is_empty(),
        "case create with the excluded purge time: expected no cluster stored, got {names:?}"
    );
}

#[tokio::test]
async fn update_cluster_keeps_every_purge_time_the_member_admits() {
    let router = emulator();
    let name = "purge-update-host";
    let (status, created) = call(&router, "CreateCluster", &create_cluster(name, Some(7))).await;
    assert_eq!(status, 200, "case setup: got {created}");

    for (case, purge_time) in ADMITTED {
        // The read in `stored_purge_time` below settles the cluster, so each row starts from
        // an ACTIVE cluster without anything waiting on the wall clock.
        let (status, updated) = call(
            &router,
            "UpdateCluster",
            &json!({
                "clusterIdentifier": name,
                "slurmConfiguration": accounting(Some(*purge_time)),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an update to {purge_time} to be served, got {status} with \
             {updated}"
        );
        assert_eq!(
            updated["cluster"]["slurmConfiguration"]["accounting"]["defaultPurgeTimeInDays"],
            json!(*purge_time),
            "case {case:?}: expected the update to answer {purge_time}, got {updated}"
        );

        let (stored, cluster_status) = stored_purge_time(&router, case, name).await;
        assert_eq!(
            stored,
            json!(*purge_time),
            "case {case:?}: expected {purge_time} to persist, got {stored}"
        );
        assert_eq!(
            cluster_status,
            json!("ACTIVE"),
            "case {case:?}: expected the cluster to settle back, got {cluster_status}"
        );
    }
}

#[tokio::test]
async fn update_cluster_refuses_the_purge_time_the_member_excludes_and_keeps_the_stored_one() {
    let router = emulator();
    let name = "purge-update-excluded";
    let (status, created) = call(&router, "CreateCluster", &create_cluster(name, Some(7))).await;
    assert_eq!(status, 200, "case setup: got {created}");
    let (stored, _) = stored_purge_time(&router, "setup", name).await;
    assert_eq!(stored, json!(7), "case setup: got {stored}");

    let (status, refused) = call(
        &router,
        "UpdateCluster",
        &json!({
            "clusterIdentifier": name,
            "slurmConfiguration": accounting(Some(EXCLUDED_PURGE_TIME_IN_DAYS)),
        }),
    )
    .await;
    assert_excluded_value_refusal("update to the excluded purge time", status, &refused);

    // Nothing was written: the stored purge time is the one the create left, and the refused
    // call did not churn the cluster to the transient status either.
    let (stored, cluster_status) =
        stored_purge_time(&router, "update to the excluded purge time", name).await;
    assert_eq!(
        stored,
        json!(7),
        "case update to the excluded purge time: expected the stored purge time untouched, got \
         {stored}"
    );
    assert_eq!(
        cluster_status,
        json!("ACTIVE"),
        "case update to the excluded purge time: expected the cluster left ACTIVE, got \
         {cluster_status}"
    );
}

#[tokio::test]
async fn a_purge_time_outside_the_members_range_is_refused_on_both_paths() {
    let router = emulator();
    let name = "purge-range-host";
    let (status, created) = call(&router, "CreateCluster", &create_cluster(name, Some(7))).await;
    assert_eq!(status, 200, "case setup: got {created}");

    for (case, purge_time) in OUTSIDE_THE_RANGE {
        let (status, refused) = call(
            &router,
            "CreateCluster",
            &create_cluster(&format!("purge-range-{case}"), Some(*purge_time)),
        )
        .await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (CLIENT_ERROR_STATUS, Some("ValidationException")),
            "case {case:?} on CreateCluster: expected {purge_time} refused, got {status} with \
             {refused}"
        );

        let (status, refused) = call(
            &router,
            "UpdateCluster",
            &json!({
                "clusterIdentifier": name,
                "slurmConfiguration": accounting(Some(*purge_time)),
            }),
        )
        .await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (CLIENT_ERROR_STATUS, Some("ValidationException")),
            "case {case:?} on UpdateCluster: expected {purge_time} refused, got {status} with \
             {refused}"
        );
    }

    // Not one of the refused calls wrote anything: only the cluster the setup created is
    // stored, and it still carries the purge time that create persisted.
    let names = cluster_names(&router).await;
    assert_eq!(
        names,
        vec![name.to_string()],
        "expected only the cluster the setup created, got {names:?}"
    );
    let (stored, _) = stored_purge_time(&router, "outside the range", name).await;
    assert_eq!(
        stored,
        json!(7),
        "case outside the range: expected the stored purge time untouched, got {stored}"
    );
}

#[tokio::test]
async fn an_update_that_names_no_purge_time_keeps_the_stored_one() {
    let router = emulator();
    let name = "purge-untouched";
    let (status, created) = call(&router, "CreateCluster", &create_cluster(name, Some(7))).await;
    assert_eq!(status, 200, "case setup: got {created}");

    // The exclusion gates the member only when the request carries it: an accounting update
    // that names just `mode` is served and leaves the stored purge time alone.
    let (status, updated) = call(
        &router,
        "UpdateCluster",
        &json!({"clusterIdentifier": name, "slurmConfiguration": accounting(None)}),
    )
    .await;
    assert_eq!(status, 200, "case mode-only update: got {updated}");

    let (stored, _) = stored_purge_time(&router, "mode-only update", name).await;
    assert_eq!(
        stored,
        json!(7),
        "case mode-only update: expected the stored purge time kept, got {stored}"
    );
}
