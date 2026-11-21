// SPDX-License-Identifier: CC0-1.0

//! How a client REACHES one Spark Connect session, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, and every session
//! is read back through `GetSession` and `ListSessions`, so these are evidence that the
//! answers are computed from persisted state.
//!
//! THE RULE: `GetSessionEndpoint` "returns the Spark Connect endpoint URL and a
//! time-limited authentication token for the specified session. Use the endpoint and token
//! to connect a PySpark client to the session. Call this operation again when the token
//! expires to obtain a new one." (its Smithy doc.) Three halves of that sentence are pinned
//! here, beside the state refusals `tests/all/session_termination.rs` owns:
//!
//! * the endpoint is an address the caller then DIALS, so it is the base this emulator is
//!   served at — never a host AWS publishes its own endpoints at, and never a function of
//!   the Region the request was signed for — with the ONE session it is asked about named
//!   under it: two sessions never share an endpoint, and every call about one session
//!   reports the same one;
//! * the token is minted for the call that asked for it, so each call answers a token of
//!   its own, no two sessions share one, and each expires a bounded span after the call —
//!   which is what "call this operation again when the token expires" asks of a client;
//! * reporting how to reach a session CHANGES nothing: however many times the endpoint is
//!   asked for, every session of the cluster is left exactly where it was, which a later
//!   `GetSession` and `ListSessions` read back.
//!
//! The rows span what the rule generalises over: how many sessions the cluster holds, which
//! cluster a session belongs to, whether two sessions were started under the same name, how
//! many times one session's endpoint is asked for, and the Region the asking request is
//! signed for.

use std::collections::BTreeSet;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::SESSION_AUTH_TOKEN_VALIDITY;

use crate::cluster_sessions::{await_session_state, launch_request, ready_cluster};
use crate::job_flow_lifecycle::{at, epoch_secs, Emr};

/// The members of a session's record that no report of its endpoint may move: its identity,
/// the state the clock walked it to, and the instants that state is stamped with.
const RECORDED_SESSION_FACTS: [&str; 7] = [
    "Id",
    "Arn",
    "ClusterId",
    "Name",
    "State",
    "CreatedAt",
    "StartedAt",
];

/// One session the rows reach: the cluster it belongs to, the name it was started under and
/// its minted id.
struct Reached {
    label: &'static str,
    cluster_id: String,
    session_id: String,
}

/// Start a session on `cluster_id`, wait for it to be up, and answer its id.
async fn started_session(emr: &Emr, cluster_id: &str, name: &str) -> String {
    let answer = emr
        .ok(
            "StartSession",
            json!({ "ClusterId": cluster_id, "Name": name }),
        )
        .await;
    let session_id = answer["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSession must answer an Id, got {answer}"))
        .to_string();
    await_session_state(emr, cluster_id, &session_id, name, "STARTED").await;
    session_id
}

/// `GetSession` for one session, as the wire reports it.
async fn get_session(emr: &Emr, cluster_id: &str, session_id: &str) -> Value {
    emr.ok(
        "GetSession",
        json!({ "ClusterId": cluster_id, "SessionId": session_id }),
    )
    .await["Session"]
        .clone()
}

/// The ids `ListSessions` reports for `cluster_id`, in the order it reports them.
async fn listed_ids(emr: &Emr, cluster_id: &str) -> Vec<String> {
    emr.ok("ListSessions", json!({ "ClusterId": cluster_id }))
        .await["Sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|session| session["Id"].as_str())
        .map(str::to_string)
        .collect()
}

/// The base an endpoint is served under: the address with the session's own path taken off
/// again, so a case reads the base this emulator reports rather than pinning a host.
fn base_of(case: &str, endpoint: &str, cluster_id: &str, session_id: &str) -> String {
    let path = format!("/clusters/{cluster_id}/sessions/{session_id}");
    endpoint
        .strip_suffix(&path)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an endpoint ending in {path:?}, got {endpoint:?}")
        })
        .to_string()
}

/// THE RULE's endpoint half, over a set of sessions: every ready session is reached at an
/// address under the ONE base this emulator is served at that names that session alone,
/// every call about it reports the same address, and no two sessions share one.
///
/// The rows differ along how many sessions the cluster holds, which cluster the session
/// belongs to, and whether two sessions carry the same name — so what the endpoint is
/// computed from is the session's recorded identity and nothing else.
#[tokio::test]
async fn each_ready_session_is_reached_at_its_own_address_under_one_served_base() {
    let emr = Emr::new();
    let case = "the endpoints of a set of sessions";
    let busy_cluster = ready_cluster(
        &emr,
        case,
        launch_request("a cluster holding several sessions", Some(true)),
    )
    .await;
    let other_cluster = ready_cluster(
        &emr,
        case,
        launch_request("a cluster holding one session", Some(true)),
    )
    .await;

    // The same NAME on two clusters, so a shared endpoint would show up here rather than
    // hiding behind two distinct names.
    let reached = [
        Reached {
            label: "the first session of a cluster holding several",
            cluster_id: busy_cluster.clone(),
            session_id: started_session(&emr, &busy_cluster, "analytics").await,
        },
        Reached {
            label: "a second session of the same cluster",
            cluster_id: busy_cluster.clone(),
            session_id: started_session(&emr, &busy_cluster, "reporting").await,
        },
        Reached {
            label: "a session of another cluster, started under a name already used",
            cluster_id: other_cluster.clone(),
            session_id: started_session(&emr, &other_cluster, "analytics").await,
        },
    ];

    let mut bases: BTreeSet<String> = BTreeSet::new();
    let mut endpoints: BTreeSet<String> = BTreeSet::new();
    let mut tokens: BTreeSet<String> = BTreeSet::new();

    for row in &reached {
        let label = row.label;
        // Asked TWICE, which is what a client whose token expired does.
        let mut seen: Vec<String> = Vec::new();
        for attempt in 1..=2 {
            let before = epoch_secs(SystemTime::now());
            let answer = emr
                .ok(
                    "GetSessionEndpoint",
                    json!({ "ClusterId": row.cluster_id, "SessionId": row.session_id }),
                )
                .await;
            let after = epoch_secs(SystemTime::now());

            let endpoint = at(&answer, &["Endpoint"])
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {label:?} attempt {attempt}: an endpoint is reported, got {answer}")
                })
                .to_string();
            let base = base_of(label, &endpoint, &row.cluster_id, &row.session_id);
            assert!(
                base.starts_with("https://") && base.matches('/').count() == 2,
                "case {label:?} attempt {attempt}: expected a scheme-and-authority base with \
                 no path of its own, got {base:?} in {answer}"
            );
            bases.insert(base);
            endpoints.insert(endpoint.clone());
            seen.push(endpoint);

            let token = at(&answer, &["AuthToken"])
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                !token.is_empty(),
                "case {label:?} attempt {attempt}: a token is reported, got {answer}"
            );
            assert!(
                tokens.insert(token),
                "case {label:?} attempt {attempt}: each call mints a token of its own, \
                 got {answer}"
            );

            // "Time-limited": the expiry is after the call that minted it and no later than
            // the bounded span this service holds a token to, measured from that call.
            let expires = at(&answer, &["AuthTokenExpirationTime"])
                .as_f64()
                .unwrap_or_else(|| {
                    panic!("case {label:?} attempt {attempt}: an expiry is reported, got {answer}")
                });
            assert!(
                expires > before,
                "case {label:?} attempt {attempt}: expected an expiry after the call at \
                 {before}, got {expires} in {answer}"
            );
            let ceiling = after + SESSION_AUTH_TOKEN_VALIDITY.as_secs_f64();
            assert!(
                expires <= ceiling,
                "case {label:?} attempt {attempt}: expected an expiry no later than {ceiling}, \
                 got {expires} in {answer}"
            );

            // The pair for a direct Spark Connect connection belongs to a server no session
            // of this emulator offers, so no session reports it.
            assert!(
                at(&answer, &["Credentials"]).is_null(),
                "case {label:?} attempt {attempt}: no credentials are reported for a \
                 connection nothing serves, got {answer}"
            );
        }
        assert_eq!(
            seen[0], seen[1],
            "case {label:?}: every call about one session reports the same endpoint, got {seen:?}"
        );
    }

    assert_eq!(
        bases.len(),
        1,
        "every session is reached under the one base this emulator is served at, got {bases:?}"
    );
    assert_eq!(
        endpoints.len(),
        reached.len(),
        "each session is reached at an address of its own, got {endpoints:?}"
    );
    for row in &reached {
        let wanted = format!(
            "{base}/clusters/{cluster}/sessions/{session}",
            base = bases.iter().next().expect("one base"),
            cluster = row.cluster_id,
            session = row.session_id
        );
        assert!(
            endpoints.contains(&wanted),
            "case {:?}: expected the session's own path under the base, got {endpoints:?}",
            row.label
        );
    }
}

/// THE RULE's reporting half: however many times a session's endpoint is asked for, the
/// call MOVES nothing — every session of the cluster is read back afterwards exactly as it
/// stood before, and the cluster's listing reports the same sessions in the same order.
#[tokio::test]
async fn reporting_a_session_endpoint_moves_no_session() {
    let emr = Emr::new();
    let case = "a cluster whose sessions are asked for their endpoints";
    let cluster_id = ready_cluster(
        &emr,
        case,
        launch_request("endpoints change nothing", Some(true)),
    )
    .await;

    // Two sessions, one of which nothing below asks about: a report that moved state would
    // show up on the asked-about session, and one that moved the cluster's whole surface on
    // the other.
    let asked = started_session(&emr, &cluster_id, "the session asked about").await;
    let untouched = started_session(&emr, &cluster_id, "a session nothing asks about").await;

    let before: Vec<(String, Value)> = vec![
        (asked.clone(), get_session(&emr, &cluster_id, &asked).await),
        (
            untouched.clone(),
            get_session(&emr, &cluster_id, &untouched).await,
        ),
    ];
    let listed_before = listed_ids(&emr, &cluster_id).await;

    // Several asks, so a counter or a state move that only a repeat would expose is covered
    // by the same rows.
    for attempt in 1..=3 {
        emr.ok(
            "GetSessionEndpoint",
            json!({ "ClusterId": cluster_id, "SessionId": asked }),
        )
        .await;
        assert_eq!(
            listed_ids(&emr, &cluster_id).await,
            listed_before,
            "attempt {attempt}: reporting an endpoint records and removes no session"
        );
    }

    for (session_id, recorded) in &before {
        let now = get_session(&emr, &cluster_id, session_id).await;
        for member in RECORDED_SESSION_FACTS {
            assert_eq!(
                at(&now, &[member]),
                at(recorded, &[member]),
                "session {session_id:?}: reporting an endpoint leaves {member} where it was, \
                 got {now} after {recorded}"
            );
        }
        assert!(
            at(&now, &["EndedAt"]).is_null(),
            "session {session_id:?}: reporting an endpoint ends no session, got {now}"
        );
    }
}

/// THE RULE's addressing half: the endpoint is where THIS emulator answers, so the Region a
/// request is signed for does not enter it — two emulators asked under two credential
/// scopes report the same base, and neither names the Region it was asked under.
#[tokio::test]
async fn the_session_endpoint_does_not_follow_the_region_the_request_is_signed_for() {
    let mut bases: BTreeSet<String> = BTreeSet::new();
    // Two Regions of different partitions' usual shape, so a base built from
    // `<service>.<region>.amazonaws.com` would differ between the rows.
    for region in ["us-east-1", "eu-west-1"] {
        let emr = Emr::with_region(region);
        let cluster_id = ready_cluster(
            &emr,
            region,
            launch_request("endpoints under a credential scope", Some(true)),
        )
        .await;
        let session_id = started_session(&emr, &cluster_id, "a session of one Region").await;
        let answer = emr
            .ok(
                "GetSessionEndpoint",
                json!({ "ClusterId": cluster_id, "SessionId": session_id }),
            )
            .await;
        let endpoint = at(&answer, &["Endpoint"])
            .as_str()
            .unwrap_or_else(|| panic!("case {region:?}: an endpoint is reported, got {answer}"))
            .to_string();
        assert!(
            !endpoint.contains(region),
            "case {region:?}: the Region the request was signed for does not enter the \
             endpoint, got {endpoint:?}"
        );
        bases.insert(base_of(region, &endpoint, &cluster_id, &session_id));
    }
    assert_eq!(
        bases.len(),
        1,
        "the endpoint names where this emulator answers, whatever Region a request is \
         signed for, got {bases:?}"
    );
}
