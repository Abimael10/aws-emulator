// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the SUPPORT WINDOW of the `DescribeClusterVersions`
//! listing, driven over the wire through the same `MultiServiceRouter` the
//! production binary mounts.
//!
//! The RULE under test is: the Kubernetes versions the listing names are the
//! versions the service still offers — a released row the clock has carried past
//! its own end of extended support is reached only by a caller that ASKS for it,
//! with `includeAll` or with a status filter naming the unsupported state — so a
//! caller that takes any version the listing names can create a cluster at it.
//!
//! Every suite asserts over a TABLE whose rows differ along what the rule
//! generalises over: where the clock sits relative to a row's own end of extended
//! support (one day either side of it and exactly on it), which ask the caller
//! makes (none, each widening one, each narrowing one), and the page size the
//! listing is walked at. The clock-relative classes are exercised by installing a
//! calendar of rows placed at chosen offsets from now, which is the only way to
//! put the clock in every class deterministically; the shipped calendar is
//! exercised as it ships.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    self, ClusterVersionRecord, CLUSTER_TYPE_EKS,
};

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{minimal_request, REGION};
use crate::wire::{call, emulator_with_engine, error_code, Answer};

/// One day, the unit the published version calendar is written in.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// `GET /cluster-versions`, with an optional query string. Every call asks for a
/// whole page, so a suite reads the listing rather than its first page.
async fn describe_versions(router: &MultiServiceRouter, query: &str) -> Answer {
    let uri = if query.is_empty() {
        "/cluster-versions?maxResults=100".to_string()
    } else {
        format!("/cluster-versions?maxResults=100&{query}")
    };
    let answer = call(router, REGION, "GET", &uri, "").await;
    assert_eq!(
        answer.status, 200,
        "case {query:?}: expected HTTP 200 from the version catalog, got {answer:?}"
    );
    answer
}

/// The `clusterVersion` of every row of an answer, in the order reported.
fn reported_versions(answer: &Answer) -> Vec<String> {
    answer.body["clusterVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a clusterVersions list, got {:?}", answer.body))
        .iter()
        .map(|row| {
            row["clusterVersion"]
                .as_str()
                .unwrap_or_else(|| panic!("expected a clusterVersion string, got {row:?}"))
                .to_string()
        })
        .collect()
}

/// One row of the calendar a suite installs, as `(version, end of standard
/// support, end of extended support, still offered at now)`.
type WindowRow = (&'static str, Duration, bool, bool);

/// The calendar the suites below install, written as offsets from now so the
/// clock sits in a known class for every row.
///
/// The rows differ along where the clock sits relative to the END OF EXTENDED
/// SUPPORT, which is the boundary this rule turns on: one day past it, exactly on
/// it, one day before it, and far either side. `(version, offset of the end of
/// extended support from now, whether that offset is in the future, still
/// offered)`.
const WINDOW_ROWS: &[WindowRow] = &[
    // Extended support ran out long ago, and one day ago.
    ("5.9", Duration::from_secs(300 * 24 * 60 * 60), false, false),
    ("5.10", DAY, false, false),
    // Exactly at the end of extended support: the window is closed at it, the
    // same way a row's status is (`endOfExtendedSupportDate` is when extended
    // support ENDS).
    ("5.11", Duration::ZERO, true, false),
    // One day of extended support left, and a row in extended support.
    ("5.12", DAY, true, true),
    ("5.13", Duration::from_secs(300 * 24 * 60 * 60), true, true),
];

/// Install [`WINDOW_ROWS`] as this service's published calendar, newest version
/// first, and answer the versions a caller is still offered and the ones that
/// have aged out.
fn seed_window_calendar(
    engine: &Arc<engine::InMemoryEngine>,
    now: SystemTime,
) -> (Vec<String>, Vec<String>) {
    let rows: Vec<ClusterVersionRecord> = WINDOW_ROWS
        .iter()
        .rev()
        .map(|(version, offset, in_future, _)| ClusterVersionRecord {
            cluster_version: (*version).to_string(),
            cluster_type: CLUSTER_TYPE_EKS.to_string(),
            default_version: false,
            release_date: now - 1000 * DAY,
            // Standard support ended before now on every row, so the class each
            // row is in is decided by its end of EXTENDED support alone.
            end_of_standard_support: now - 500 * DAY,
            end_of_extended_support: if *in_future {
                now + *offset
            } else {
                now - *offset
            },
        })
        .collect();
    let store = state::service_state(&**engine).expect("the eks store is reachable in-process");
    store.lock().cluster_versions = rows;
    let offered: Vec<String> = WINDOW_ROWS
        .iter()
        .rev()
        .filter(|(_, _, _, offered)| *offered)
        .map(|(version, _, _, _)| (*version).to_string())
        .collect();
    let aged_out: Vec<String> = WINDOW_ROWS
        .iter()
        .rev()
        .filter(|(_, _, _, offered)| !*offered)
        .map(|(version, _, _, _)| (*version).to_string())
        .collect();
    (offered, aged_out)
}

/// The listing a caller reads with each ask: unasked and with every narrowing
/// form it carries the offered rows alone, and only the widening asks reach the
/// rows that have aged out.
#[tokio::test]
async fn only_a_widening_ask_reaches_a_version_past_extended_support() {
    let (router, engine) = emulator_with_engine();
    let now = SystemTime::now();
    let (offered, aged_out) = seed_window_calendar(&engine, now);

    // (case, query, expected versions in the order reported).
    let widened: Vec<String> = WINDOW_ROWS
        .iter()
        .rev()
        .map(|(version, _, _, _)| (*version).to_string())
        .collect();
    let cases: Vec<(&str, String, Vec<String>)> = vec![
        ("no ask at all", String::new(), offered.clone()),
        (
            "the widening flag",
            "includeAll=true".to_string(),
            widened.clone(),
        ),
        (
            "the widening flag, off",
            "includeAll=false".to_string(),
            offered.clone(),
        ),
        (
            "the current spelling of the status filter naming the unsupported state",
            "versionStatus=UNSUPPORTED".to_string(),
            aged_out.clone(),
        ),
        (
            "the deprecated spelling naming it",
            "status=unsupported".to_string(),
            aged_out.clone(),
        ),
        (
            "a status filter naming a supported state",
            "versionStatus=EXTENDED_SUPPORT".to_string(),
            offered.clone(),
        ),
        (
            // Naming a version says nothing about its lifecycle state, so it
            // narrows the listing rather than widening it.
            "a version filter naming a row that has aged out",
            format!("clusterVersions={}", aged_out[0]),
            Vec::new(),
        ),
        (
            "that same version filter, widened",
            format!("includeAll=true&clusterVersions={}", aged_out[0]),
            vec![aged_out[0].clone()],
        ),
        (
            "the published cluster type",
            format!("clusterType={CLUSTER_TYPE_EKS}"),
            offered.clone(),
        ),
        (
            "the default-only filter, off",
            "defaultOnly=false".to_string(),
            offered.clone(),
        ),
    ];

    for (case, query, expected) in cases {
        let actual = reported_versions(&describe_versions(&router, &query).await);
        assert_eq!(
            actual, expected,
            "case {case}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// The rule holds for the listing as a WHOLE, whatever page size it is walked at:
/// a narrowing applied per page would otherwise let a row that has aged out
/// through on a later page.
#[tokio::test]
async fn no_page_of_the_unasked_listing_names_a_version_past_extended_support() {
    let (router, engine) = emulator_with_engine();
    let now = SystemTime::now();
    let (offered, aged_out) = seed_window_calendar(&engine, now);

    for page_size in [1usize, 2, 3, 100] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let query = match &token {
                None => format!("maxResults={page_size}"),
                Some(next) => format!("maxResults={page_size}&nextToken={next}"),
            };
            let answer = call(
                &router,
                REGION,
                "GET",
                &format!("/cluster-versions?{query}"),
                "",
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case page size {page_size}: expected HTTP 200, got {answer:?}"
            );
            let page = reported_versions(&answer);
            assert!(
                page.len() <= page_size,
                "case page size {page_size}: a page must not exceed maxResults, got {page:?}"
            );
            for version in &page {
                assert!(
                    !aged_out.contains(version),
                    "case page size {page_size}: expected no version past extended support, got \
                     {version} among {page:?}"
                );
            }
            walked.extend(page);
            token = answer
                .body
                .get("nextToken")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            if token.is_none() {
                break;
            }
            assert!(
                walked.len() <= offered.len(),
                "case page size {page_size}: paging must terminate, walked {walked:?}"
            );
        }
        assert_eq!(
            walked, offered,
            "case page size {page_size}: expected the offered rows once and in order, got \
             {walked:?}"
        );
    }
}

/// What the listing names is what `CreateCluster` accepts: every version the
/// unasked listing carries creates a cluster, and every version only the widened
/// listing carries is refused — which is what makes the catalogue and the create
/// path agree about the floor a caller may plan from.
#[tokio::test]
async fn every_version_the_listing_names_creates_a_cluster() {
    let (router, engine) = emulator_with_engine();
    let now = SystemTime::now();
    let (offered, aged_out) = seed_window_calendar(&engine, now);

    // (case, version, whether a cluster can be created at it). Both ends of the
    // listing are rows here, the oldest one included: a planner that takes the
    // lowest version the listing names must be able to create at it.
    let cases: Vec<(String, String, bool)> = offered
        .iter()
        .map(|version| (format!("offered {version}"), version.clone(), true))
        .chain(
            aged_out
                .iter()
                .map(|version| (format!("aged out {version}"), version.clone(), false)),
        )
        .collect();

    for (case, version, creatable) in cases {
        let name = format!("window-{}", version.replace('.', "-"));
        let mut body = minimal_request(&name);
        body["version"] = serde_json::Value::String(version.clone());
        let created = call(&router, REGION, "POST", "/clusters", &body.to_string()).await;
        if creatable {
            assert_eq!(
                created.status, 200,
                "case {case}: a version the listing names must create a cluster, got {created:?}"
            );
            assert_eq!(
                created.cluster()["version"].as_str(),
                Some(version.as_str()),
                "case {case}: the cluster must run the version asked for, got {:?}",
                created.cluster()["version"]
            );
        } else {
            assert_eq!(
                created.status, 400,
                "case {case}: a version past extended support creates no cluster, got {created:?}"
            );
            assert_eq!(
                error_code(&created),
                "InvalidParameterException",
                "case {case}: expected InvalidParameterException, got {created:?}"
            );
            assert_eq!(
                created.message(),
                // Written out rather than read from the constant the handler
                // renders it from, so this check fails if the wording changes.
                format!("unsupported Kubernetes version {version}"),
                "case {case}: expected the refusal to name the version, got {:?}",
                created.message()
            );
        }
    }
}

/// The same rule on the calendar AS SHIPPED: the oldest version the listing names
/// creates a cluster, the two supported states cover that listing exactly, and the
/// widened listing is the one that carries the rows which have aged out.
#[tokio::test]
async fn the_shipped_listing_is_creatable_from_its_oldest_row() {
    let (router, _engine) = emulator_with_engine();

    let listed = reported_versions(&describe_versions(&router, "").await);
    assert!(
        listed.len() >= 2,
        "case the shipped listing: expected a choice of versions, got {listed:?}"
    );

    // The rows a caller discovers, both ends of the listing included: an upgrade
    // planner walks it and plans from the oldest row it names.
    let oldest = listed
        .last()
        .expect("the listing carries at least one row")
        .clone();
    let name = format!("shipped-{}", oldest.replace('.', "-"));
    let mut body = minimal_request(&name);
    body["version"] = serde_json::Value::String(oldest.clone());
    let created = call(&router, REGION, "POST", "/clusters", &body.to_string()).await;
    assert_eq!(
        created.status, 200,
        "case oldest listed {oldest}: the lowest version the listing names must be creatable, \
         got {created:?}"
    );

    // The two supported states cover the listing read unasked, and the widened
    // listing is the one that also carries the rows past extended support.
    let mut covered: Vec<String> = Vec::new();
    for status in ["STANDARD_SUPPORT", "EXTENDED_SUPPORT"] {
        covered.extend(reported_versions(
            &describe_versions(&router, &format!("versionStatus={status}")).await,
        ));
    }
    covered.sort();
    let mut unasked = listed.clone();
    unasked.sort();
    assert_eq!(
        covered, unasked,
        "case the status partition: expected the two supported states to cover the listing \
         {unasked:?}, got {covered:?}"
    );
    let aged_out =
        reported_versions(&describe_versions(&router, "versionStatus=UNSUPPORTED").await);
    assert!(
        !aged_out.is_empty(),
        "case the shipped calendar: it must carry a version past extended support, so leaving \
         those rows out of the listing has something to prove, got {listed:?}"
    );
    let widened = reported_versions(&describe_versions(&router, "includeAll=true").await);
    for version in &aged_out {
        assert!(
            widened.contains(version) && !listed.contains(version),
            "case aged out {version}: expected it in the widened listing {widened:?} and not in \
             {listed:?}"
        );
    }
}
