// SPDX-License-Identifier: CC0-1.0

//! `StartInsightsRefresh` refuses a second refresh while the cluster's own refresh
//! still runs the way Amazon EKS does: with the refusal the plane that runs the
//! analysis answered, carried through WRAPPED.
//!
//! The RULE under test: a refusal Amazon EKS serves out of its internal
//! cluster-insights plane reports that plane's own FIXED sentence followed by the
//! tail that names the call behind it — the service that answered, the status it
//! answered with, the id of that call and how many attempts were made — and
//! interpolates nothing of the request or of the running work into it. For a second
//! on-demand refresh that sentence is `Another refresh operation is already in
//! progress for the cluster.` and the relayed call answered `409`, so the whole
//! message is that sentence followed by `(Service: EksClusterInsights, Status Code:
//! 409, Request ID: <the id of that call>) (SDK Attempt Count: 1)`. Only that last
//! id varies, because a request id identifies the call that was made: it is drawn
//! afresh per call and two refusals never carry the same one. The refusal itself is
//! the modelled `InvalidRequestException` (400) — the status of THIS call, which the
//! relayed status in the tail does not change.
//!
//! The rows differ along what the rule generalises over: the SHAPE and LENGTH of the
//! cluster name the refusal is earned under (a short name, a name carrying digits and
//! separators, the longest name this API accepts), and how many extra starts the
//! caller piles on — every one of them is refused with the same sentence and its own
//! relayed id.
//!
//! The contrast row is the first start of a cluster that is running nothing: it is
//! served, so no wrapped refusal is composed for it at all.
//!
//! The wire harness is the shared one (`tests/all/wire.rs`); so are the UUID shape
//! check and the create-and-poll-until-ACTIVE driver
//! (`tests/all/certificate_authority.rs`, `tests/all/certificate_authority_rotation.rs`),
//! which this suite reuses rather than keeping a copy of.

use std::collections::BTreeSet;

use service_com_amazonaws_eks_awswesleyfrontend::state::MAX_CLUSTER_NAME_LEN;
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_authority::{is_uuid, REGION};
use crate::certificate_authority_rotation::active_cluster;
use crate::wire::{call, emulator, encode_label, error_code, Answer, POLL_DEADLINE};

/// The sentence the cluster-insights plane refuses a second refresh with. Written out
/// here rather than composed from the service's own constants, so a change to that
/// wording cannot keep this suite green.
const CONFLICT_SENTENCE: &str = "Another refresh operation is already in progress for the cluster.";

/// The clause that opens the tail of a refusal served out of that plane, up to the id
/// of the relayed call. The status in it is the one THAT call answered, not the 400
/// this operation answers its own caller with.
const PLANE_CLAUSE: &str = " (Service: EksClusterInsights, Status Code: 409, Request ID: ";

/// What closes it: how many attempts were made to reach that plane.
const PLANE_TAIL: &str = ") (SDK Attempt Count: 1)";

/// `StartInsightsRefresh`: `POST /clusters/{clusterName}/insights-refresh`.
async fn start_refresh(router: &MultiServiceRouter, cluster: &str) -> Answer {
    call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/insights-refresh", encode_label(cluster)),
        "",
    )
    .await
}

/// `DescribeInsightsRefresh`: `GET /clusters/{clusterName}/insights-refresh`.
async fn describe_refresh(router: &MultiServiceRouter, cluster: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!("/clusters/{}/insights-refresh", encode_label(cluster)),
        "",
    )
    .await
}

/// A timestamp member of an answer, as the epoch-seconds number restJson1 carries.
fn instant(case: &str, answer: &Answer, member: &str) -> f64 {
    answer.body[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a numeric {member} in {answer:?}"))
}

/// The id of the relayed call a wrapped refusal carries, asserted to be the plane's
/// fixed sentence followed by that tail and nothing else.
fn relayed_request_id(case: &str, actual: &str) -> String {
    let head = format!("{CONFLICT_SENTENCE}{PLANE_CLAUSE}");
    assert!(
        actual.starts_with(&head),
        "case {case:?}: expected the refusal to begin {head:?}, got {actual:?}"
    );
    assert!(
        actual.ends_with(PLANE_TAIL),
        "case {case:?}: expected the refusal to end {PLANE_TAIL:?}, got {actual:?}"
    );
    let id = actual[head.len()..actual.len() - PLANE_TAIL.len()].to_string();
    assert!(
        is_uuid(&id),
        "case {case:?}: expected the relayed call's id to be a UUID, got {id:?}"
    );
    id
}

/// Start a refresh on `cluster` and immediately start another, answering the refresh
/// the first start began (as `DescribeInsightsRefresh` reports it) and the refusal the
/// second earned.
///
/// A SERVED second start means the first refresh had already ended, so that start is
/// the beginning of the next refresh and the pair is judged again from it — never by
/// issuing a further first start, which the refresh it just began would refuse. The
/// condition under test is "a start while one runs", so it is waited for by looking
/// again rather than by racing a span. A generous deadline bounds the wait; nothing
/// here asserts how fast the host runs.
async fn conflicting_start(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
) -> (Answer, Answer) {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    let mut started = start_refresh(router, cluster).await;
    loop {
        assert_eq!(
            started.status, 200,
            "case {case:?}: expected 200 from the start that begins a refresh, got {started:?}"
        );
        let running = describe_refresh(router, cluster).await;
        let again = start_refresh(router, cluster).await;
        if again.status != 200 {
            return (running, again);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: no second start ever landed on a running refresh, last {again:?}"
        );
        started = again;
    }
}

/// The longest name this API accepts, built from the maximum the model states so the
/// row cannot drift from it.
fn longest_cluster_name() -> String {
    let prefix = "conflict-wording-";
    format!(
        "{prefix}{}",
        "c".repeat(MAX_CLUSTER_NAME_LEN - prefix.len())
    )
}

// ---------------------------------------------------------------------------
// RULE: a second `StartInsightsRefresh` while the cluster's refresh still runs is
// refused 400 `InvalidRequestException` with the cluster-insights plane's own fixed
// sentence followed by the tail naming the relayed call, whose only variable part is
// that call's own id — and the running refresh is left exactly as it was.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_start_while_a_refresh_runs_is_refused_in_the_insights_plane_wording() {
    let longest = longest_cluster_name();
    // Rows differ along the shape and length of the cluster name the refusal is
    // earned under: none of it may reach the sentence.
    let cases: Vec<(&str, String)> = vec![
        ("a short name", "conflict".to_string()),
        (
            "digits and separators",
            "sim-conflict-7-0f3a_9".to_string(),
        ),
        ("the longest name this API accepts", longest),
    ];

    let mut ids: BTreeSet<String> = BTreeSet::new();
    for (case, name) in &cases {
        let router = emulator();
        active_cluster(&router, case, name).await;

        let (running, refused) = conflicting_start(&router, case, name).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400 from the second StartInsightsRefresh, got {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        let message = refused.message();
        let id = relayed_request_id(case, &message);
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: the relayed call's id must be drawn afresh, saw {id:?} twice"
        );

        // Nothing of the request or of the running refresh is interpolated into the
        // sentence: the whole message is the fixed text plus that one id.
        assert_eq!(
            message,
            format!("{CONFLICT_SENTENCE}{PLANE_CLAUSE}{id}{PLANE_TAIL}"),
            "case {case:?}: the message is the plane's sentence plus the relayed tail only"
        );
        assert!(
            !message.contains(name.as_str()),
            "case {case:?}: the sentence must not name the cluster, got {message:?}"
        );
        for leaked in ["IN_PROGRESS", "startedAt", "wait for it"] {
            assert!(
                !message.contains(leaked),
                "case {case:?}: the sentence must not carry {leaked:?}, got {message:?}"
            );
        }

        // The refusal reports the cluster the request addressed in its own member, as
        // every refusal of this operation does — that is the payload, not the text.
        assert_eq!(
            refused.body["clusterName"],
            serde_json::json!(name),
            "case {case:?}: the refusal names the cluster in its payload, got {refused:?}"
        );

        // The refused start changed nothing: the refresh that was running is still
        // the one the cluster reports, under its own unchanged startedAt.
        assert_eq!(
            running.body["status"],
            serde_json::json!("IN_PROGRESS"),
            "case {case:?}: the first start must leave a running refresh, got {running:?}"
        );
        let started_at = instant(case, &running, "startedAt");
        let after = describe_refresh(&router, name).await;
        assert_eq!(
            instant(case, &after, "startedAt"),
            started_at,
            "case {case:?}: a refused start must not restart the running refresh, got {after:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE (same rule, generalised over the COUNT): every extra start lands on the same
// refusal, each with its own relayed call id — the wording is a property of the
// refusal, not of the first one.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_extra_start_earns_the_same_sentence_with_its_own_relayed_id() {
    // Rows differ along how many extra starts the caller piles on.
    let cases: Vec<(&str, usize)> = vec![("two extra starts", 2), ("five extra starts", 5)];

    for (index, (case, extra)) in cases.iter().enumerate() {
        let router = emulator();
        let name = format!("conflict-rounds-{index}");
        active_cluster(&router, case, &name).await;

        let (_, first) = conflicting_start(&router, case, &name).await;
        let mut ids: BTreeSet<String> = BTreeSet::new();
        ids.insert(relayed_request_id(case, &first.message()));

        for attempt in 2..=*extra {
            let refused = start_refresh(&router, &name).await;
            // The refresh is a clock-owed span, so a later start may legitimately
            // land after it ended: only a REFUSED one is this rule's subject, and a
            // served one proves nothing about the wording either way.
            if refused.status == 200 {
                continue;
            }
            assert_eq!(
                refused.status, 400,
                "case {case:?}: start {attempt} must be refused with 400, got {refused:?}"
            );
            assert_eq!(
                error_code(&refused),
                "InvalidRequestException",
                "case {case:?}: start {attempt} expected InvalidRequestException, got {refused:?}"
            );
            let id = relayed_request_id(case, &refused.message());
            assert!(
                ids.insert(id.clone()),
                "case {case:?}: start {attempt} repeated the relayed id {id:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CONTRAST: a cluster running nothing is SERVED, so no wrapped refusal is composed
// for it — the tail says which plane answered, and a served start had nothing to
// relay.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_first_start_is_served_and_carries_no_relayed_tail() {
    let case = "a cluster running nothing";
    let router = emulator();
    let name = "conflict-contrast";
    active_cluster(&router, case, name).await;

    let started = start_refresh(&router, name).await;
    assert_eq!(
        started.status, 200,
        "case {case:?}: expected 200 from the first StartInsightsRefresh, got {started:?}"
    );
    assert_eq!(
        started.body["status"],
        serde_json::json!("IN_PROGRESS"),
        "case {case:?}: the first start begins a refresh IN_PROGRESS, got {started:?}"
    );
    // The served answer carries the refresh's OWN message, so the check below has
    // something to catch: it is the message that must be free of the relayed text,
    // not an absent member.
    let message = started.body["message"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: a served start reports the refresh's message, got {started:?}")
    });
    assert!(
        !message.is_empty(),
        "case {case:?}: the served message must not be empty, got {started:?}"
    );
    for leaked in ["Service:", "SDK Attempt Count", CONFLICT_SENTENCE] {
        assert!(
            !message.contains(leaked),
            "case {case:?}: a served start carries no relayed {leaked:?}, got {message:?}"
        );
    }
}
