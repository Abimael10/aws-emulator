// SPDX-License-Identifier: CC0-1.0

//! The EXPIRY a presigned application-UI URL carries, over both surfaces that hand one out,
//! end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends.
//!
//! THE RULE: a URL `GetOnClusterAppUIPresignedURL` or `GetPersistentAppUIPresignedURL`
//! hands back is PRESIGNED, so the credential in it ages out and the instant it ages out at
//! is DISCOVERABLE FROM THE URL ITSELF: the URL states when it was signed and how many
//! whole seconds it stays valid, their sum is an instant in the future of the call that
//! answered it, and the window between them is bounded — a caller can therefore tell when to
//! mint a fresh URL, and a URL that was cached or forwarded stops opening the interface
//! instead of opening it for ever. Both surfaces answer with the SAME window, because one
//! builder presigns for both.
//!
//! The rows span what the rule generalises over: which operation answered, the interface the
//! request asks for (every `OnClusterAppUIType` the model declares, and the UI each
//! `ProfilerType` the create accepts publishes), whether the request names an application,
//! and how many times it is repeated — the expiry is a property of every answer, not of one.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MAX_SIGNED_URL_EXPIRY;

use crate::job_flow_lifecycle::{at, kept_alive_request, Emr};
use crate::on_cluster_app_ui::query_fields;

/// Every `OnClusterAppUIType` the model declares, which the on-cluster surface is asked for
/// one of. SOURCE: `smithy_query(query="shape", shape="OnClusterAppUIType")` —
/// `SparkHistoryServer | YarnTimelineService | TezUI | ApplicationMaster | JobHistoryServer |
/// ResourceManager`.
const ON_CLUSTER_UI_TYPES: [&str; 6] = [
    "SparkHistoryServer",
    "YarnTimelineService",
    "TezUI",
    "ApplicationMaster",
    "JobHistoryServer",
    "ResourceManager",
];

/// Every `ProfilerType` `CreatePersistentAppUI` accepts, with the `PersistentAppUIType` the
/// user interface it creates is addressed by. SOURCE: `smithy_query(query="shape",
/// shape="ProfilerType,PersistentAppUIType")` — `ProfilerType` is `SHS | TEZUI | YTS` and
/// `PersistentAppUIType` is `SHS | TEZ | YTS`.
const PROFILER_TO_UI_TYPE: [(&str, &str); 3] = [("SHS", "SHS"), ("TEZUI", "TEZ"), ("YTS", "YTS")];

/// The applications a cluster of this suite runs, so every interface above has an
/// application to serve it and every row is answered a URL.
const APPLICATIONS: [&str; 3] = ["Spark", "Hadoop", "Tez"];

/// The longest window a presigned application-UI credential may still be called bounded.
///
/// A CHOSEN ceiling, and a generous one: Amazon EMR publishes no validity for an
/// application-UI presigned URL, so what this asserts is the class the answer belongs to —
/// a credential that ages out within a day of being signed rather than one that outlives the
/// session it was minted for. It is far below the longest validity this emulator's signer
/// accepts at all ([`MAX_SIGNED_URL_EXPIRY`], seven days), which is what makes the assertion
/// a statement about this service rather than about the signer.
const LONGEST_BOUNDED_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// How often a persistent user interface's readiness is looked at again while it is still
/// starting.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A generous deadline for the clock-owed attach of a persistent user interface: the wait is
/// on the CONDITION (the interface reporting a URL as ready), so this only has to be far past
/// the span the service owes that transition on any host.
const ATTACH_DEADLINE: Duration = Duration::from_secs(30);

/// How many times each row asks for a URL.
const REPEATS: usize = 2;

/// The field naming the instant a presigned URL was signed, and the one naming the whole
/// seconds it stays valid from that instant — the two halves of the expiry a SigV4
/// query-signed URL carries.
const SIGNED_AT_FIELD: &str = "X-Amz-Date";
const VALID_FOR_FIELD: &str = "X-Amz-Expires";

/// The instant `X-Amz-Date` names, in epoch seconds: a SigV4 date is the basic ISO 8601 form
/// `YYYYMMDDTHHMMSSZ`, always UTC.
fn signed_at_epoch(case: &str, value: &str) -> i64 {
    ::chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {SIGNED_AT_FIELD} is an ISO 8601 basic UTC instant, got {value:?} ({error})")
        })
        .and_utc()
        .timestamp()
}

/// Now, in epoch seconds.
fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs() as i64
}

/// THE RULE on one URL: the expiry is discoverable from the URL, it lies in the future of the
/// call that answered it, and the window it bounds is a bounded one. Answers the window, so
/// the caller can hold the two surfaces to the same one.
fn assert_bounded_future_expiry(case: &str, url: &str, asked_at: i64) -> u64 {
    let fields = query_fields(case, url);
    let signed_at = fields.get(SIGNED_AT_FIELD).unwrap_or_else(|| {
        panic!("case {case:?}: a presigned URL states when it was signed, got {url}")
    });
    let signed_at = signed_at_epoch(case, signed_at);
    let valid_for = fields
        .get(VALID_FOR_FIELD)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a presigned URL states how long it stays valid, got {url}")
        })
        .parse::<u64>()
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {VALID_FOR_FIELD} counts whole seconds, got {error} in {url}")
        });

    // The window is bounded: at least a second, and inside the ceiling such a credential
    // must age out within.
    assert!(
        valid_for > 0 && Duration::from_secs(valid_for) <= LONGEST_BOUNDED_WINDOW,
        "case {case:?}: expected a bounded window (0 < {valid_for}s <= {}s), got {url}",
        LONGEST_BOUNDED_WINDOW.as_secs()
    );
    assert!(
        Duration::from_secs(valid_for) <= MAX_SIGNED_URL_EXPIRY,
        "case {case:?}: a window no signer would accept is no window, got {url}"
    );
    // It was signed when it was answered, not at some instant of its own.
    assert!(
        signed_at >= asked_at - 60,
        "case {case:?}: expected the URL to be signed by the call that answered it (at or \
         after {asked_at}), got {signed_at} in {url}"
    );
    // And the instant it ages out at is still ahead: a caller that reads the expiry off the
    // URL is told a future instant, which is what makes it something to schedule against.
    let expires_at = signed_at + valid_for as i64;
    assert!(
        expires_at > now_epoch(),
        "case {case:?}: expected an expiry in the future, got {expires_at} in {url}"
    );
    valid_for
}

/// A cluster that stays up, running every application whose interface a row asks for.
async fn running_cluster(emr: &Emr, name: &str) -> (String, String) {
    let mut request = kept_alive_request(name);
    request["ReleaseLabel"] = json!("emr-6.15.0");
    request["Applications"] = json!(APPLICATIONS
        .iter()
        .map(|name| json!({ "Name": name }))
        .collect::<Vec<Value>>());
    let out = emr.ok("RunJobFlow", request).await;
    let id = out["JobFlowId"]
        .as_str()
        .expect("RunJobFlow must answer a JobFlowId")
        .to_string();
    let arn = out["ClusterArn"]
        .as_str()
        .expect("RunJobFlow must answer a ClusterArn")
        .to_string();
    (id, arn)
}

/// Look at `GetPersistentAppUIPresignedURL` again until the interface reports a URL as ready —
/// the condition the clock-owed attach makes true — or a generous deadline passes.
async fn await_ready(emr: &Emr, case: &str, id: &str, ui_type: &str) {
    let deadline = std::time::Instant::now() + ATTACH_DEADLINE;
    loop {
        let out = emr
            .ok(
                "GetPersistentAppUIPresignedURL",
                json!({ "PersistentAppUIId": id, "PersistentAppUIType": ui_type }),
            )
            .await;
        if at(&out, &["PresignedURLReady"]).as_bool() == Some(true) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the interface never reported a ready URL, last {out}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The URL an answer carries, which a row that is ready must have.
fn url_of(case: &str, out: &Value) -> String {
    assert_eq!(
        at(out, &["PresignedURLReady"]).as_bool(),
        Some(true),
        "case {case:?}: the interface asked for is served by this cluster, got {out}"
    );
    at(out, &["PresignedURL"])
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no URL in {out}"))
        .to_string()
}

/// THE RULE over the whole table: every URL either surface hands back carries a bounded
/// future expiry, and both surfaces carry the same window.
#[tokio::test]
async fn every_presigned_app_ui_url_carries_a_bounded_future_expiry() {
    let emr = Emr::new();
    let (cluster_id, cluster_arn) = running_cluster(&emr, "presigned-expiry").await;

    // Every persistent interface is created before any URL is asked for, so each one's
    // clock-owed attach runs beside the others rather than after it.
    let mut persistent: Vec<(&str, &str, String)> = Vec::new();
    for (profiler, ui_type) in PROFILER_TO_UI_TYPE {
        let out = emr
            .ok(
                "CreatePersistentAppUI",
                json!({ "TargetResourceArn": &cluster_arn, "ProfilerType": profiler }),
            )
            .await;
        let id = out["PersistentAppUIId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {profiler:?}: no PersistentAppUIId in {out}"))
            .to_string();
        persistent.push((profiler, ui_type, id));
    }
    for (profiler, ui_type, id) in &persistent {
        await_ready(&emr, profiler, id, ui_type).await;
    }

    // Every window this test ever reads, so "one window for both surfaces" is asserted across
    // the whole table rather than within one row.
    let mut windows: BTreeSet<u64> = BTreeSet::new();

    for application_id in [None, Some("application_1700000000000_0001")] {
        // The on-cluster surface: one row per interface the model declares.
        for ui_type in ON_CLUSTER_UI_TYPES {
            let case =
                format!("GetOnClusterAppUIPresignedURL / {ui_type} / application {application_id:?}");
            let mut request = json!({ "ClusterId": &cluster_id, "OnClusterAppUIType": ui_type });
            if let Some(application_id) = application_id {
                request["ApplicationId"] = json!(application_id);
            }
            for repeat in 0..REPEATS {
                let case = format!("{case} (call {repeat})");
                let asked_at = now_epoch();
                let out = emr
                    .ok("GetOnClusterAppUIPresignedURL", request.clone())
                    .await;
                let url = url_of(&case, &out);
                windows.insert(assert_bounded_future_expiry(&case, &url, asked_at));
            }
        }

        // The persistent surface: one row per interface a profiler publishes.
        for (profiler, ui_type, id) in &persistent {
            let case = format!(
                "GetPersistentAppUIPresignedURL / {profiler} as {ui_type} / application \
                 {application_id:?}"
            );
            let mut request = json!({ "PersistentAppUIId": id, "PersistentAppUIType": ui_type });
            if let Some(application_id) = application_id {
                request["ApplicationId"] = json!(application_id);
            }
            for repeat in 0..REPEATS {
                let case = format!("{case} (call {repeat})");
                let asked_at = now_epoch();
                let out = emr
                    .ok("GetPersistentAppUIPresignedURL", request.clone())
                    .await;
                let url = url_of(&case, &out);
                windows.insert(assert_bounded_future_expiry(&case, &url, asked_at));
            }
        }
    }

    assert_eq!(
        windows.len(),
        1,
        "both presigned-URL surfaces state the same validity, got {windows:?}"
    );
}

/// THE RULE's other half: an answer that carries NO URL carries no expiry to read either, so
/// a dry run — the one answer of a served interface that mints nothing — is not a credential
/// whose expiry a caller has to go looking for.
///
/// The rows span the interfaces of the on-cluster surface, which is the one whose request
/// carries `DryRun`.
#[tokio::test]
async fn a_dry_run_hands_back_no_url_and_so_no_expiry() {
    let emr = Emr::new();
    let (cluster_id, _) = running_cluster(&emr, "presigned-expiry-dry").await;

    for ui_type in ON_CLUSTER_UI_TYPES {
        let case = format!("GetOnClusterAppUIPresignedURL / {ui_type} / DryRun");
        let out = emr
            .ok(
                "GetOnClusterAppUIPresignedURL",
                json!({
                    "ClusterId": &cluster_id,
                    "OnClusterAppUIType": ui_type,
                    "DryRun": true,
                }),
            )
            .await;
        assert_eq!(
            at(&out, &["PresignedURLReady"]).as_bool(),
            Some(true),
            "case {case:?}: a served interface is ready for a dry run too, got {out}"
        );
        assert!(
            at(&out, &["PresignedURL"]).is_null(),
            "case {case:?}: a dry run hands back no URL, got {out}"
        );
    }
}
