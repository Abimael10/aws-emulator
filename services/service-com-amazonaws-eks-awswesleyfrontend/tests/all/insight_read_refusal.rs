// SPDX-License-Identifier: CC0-1.0

//! `DescribeInsight` refuses an insight the named cluster has none of the way
//! Amazon EKS does: with the refusal the plane that runs the checks answered,
//! carried through WRAPPED.
//!
//! The RULE under test: a read Amazon EKS serves out of an internal plane reports
//! that plane's own refusal followed by the tail that names the call behind it — the
//! service that answered, the status it answered with, the id of that call and how
//! many attempts were made. For a cluster's insights that plane is
//! `EksClusterInsights`, so an `id` that is not one of the cluster's insights is
//! `Cannot find insight insightId=<the id the request carried>
//! clusterId=<the id the cluster publishes for itself> (Service: EksClusterInsights,
//! Status Code: 404, Request ID: <the id of that call>) (SDK Attempt Count: 1)`.
//! Everything but that last id is fixed text or taken from the request and the
//! store; the request id identifies the call that was made, so it is drawn afresh
//! per call and two refusals never carry the same one.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the id that
//! resolves to no insight (a well-formed UUID nothing analysed, a real insight id in
//! the wrong case, a value that is no UUID, one carrying characters path encoding
//! must carry through, a single character, a long one), and the CLUSTER the read is
//! addressed to — a second cluster in the same region and a same-named cluster in
//! another region, each reporting its own id.
//!
//! The contrast row is the refusal of the Amazon EKS control plane itself: a
//! `clusterName` the region does not hold is that plane's own `ResourceNotFound`,
//! so it carries no wrapped tail — the tail says which plane answered, and inventing
//! one for a refusal this service composed itself would misattribute it.
//!
//! The wire harness and the cluster drivers are the shared ones
//! (`tests/all/wire.rs`, `tests/all/cluster_lifecycle.rs`).

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_authority::is_uuid;
use crate::cluster_lifecycle::minimal_request;
use crate::wire::{call, emulator, encode_label, error_code, read_until, Answer};

/// The region the rows are signed for, and the second one a same-named cluster is
/// created in.
const REGION: &str = "us-west-2";
/// See [`REGION`].
const OTHER_REGION: &str = "eu-central-1";

/// The clause that opens the tail of a refusal served out of the cluster-insights
/// plane, up to the id of the call. Written out here rather than composed from the
/// service's own constants, so a change to that wording cannot keep this suite
/// green.
const PLANE_CLAUSE: &str = " (Service: EksClusterInsights, Status Code: 404, Request ID: ";

/// What closes it: how many attempts were made to reach that plane.
const PLANE_TAIL: &str = ") (SDK Attempt Count: 1)";

/// `CreateCluster` in `region`, asserted to be accepted.
async fn create_cluster(router: &MultiServiceRouter, region: &str, name: &str) {
    let created = call(
        router,
        region,
        "POST",
        "/clusters",
        &minimal_request(name).to_string(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: expected 200 from CreateCluster for {name:?} in {region:?}, got {created:?}"
    );
}

/// `DescribeCluster` in `region`.
async fn describe_cluster(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "GET",
        &format!("/clusters/{}", encode_label(name)),
        "",
    )
    .await
}

/// `DescribeInsight` in `region`: `GET /clusters/{clusterName}/insights/{id}`.
async fn describe_insight(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    id: &str,
) -> Answer {
    call(
        router,
        region,
        "GET",
        &format!(
            "/clusters/{}/insights/{}",
            encode_label(cluster),
            encode_label(id)
        ),
        "",
    )
    .await
}

/// `ListInsights` in `region`, unfiltered: `POST /clusters/{clusterName}/insights`.
async fn list_insights(router: &MultiServiceRouter, region: &str, cluster: &str) -> Answer {
    call(
        router,
        region,
        "POST",
        &format!("/clusters/{}/insights", encode_label(cluster)),
        "{}",
    )
    .await
}

/// The id the cluster publishes FOR ITSELF, read where a caller can see it: the last
/// segment of its OIDC issuer URL. Waits for the cluster to reach `ACTIVE` by
/// LOOKING AGAIN, since that is when the issuer is published.
async fn published_cluster_id(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    cluster: &str,
) -> String {
    let described = read_until(
        case,
        "the cluster reported ACTIVE with its OIDC issuer",
        || describe_cluster(router, region, cluster),
        |answer| {
            answer.cluster()["status"] == serde_json::json!("ACTIVE")
                && answer.cluster()["identity"]["oidc"]["issuer"].is_string()
        },
    )
    .await;
    let issuer = described.cluster()["identity"]["oidc"]["issuer"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    issuer
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Asserts `actual` is `sentence` followed by the plane's tail, and answers the id
/// of the relayed call it carries.
fn relayed_request_id(case: &str, sentence: &str, actual: &str) -> String {
    let head = format!("{sentence}{PLANE_CLAUSE}");
    assert!(
        actual.starts_with(&head),
        "case {case:?}: expected the refusal to begin {head:?}, got {actual:?}"
    );
    assert!(
        actual.ends_with(PLANE_TAIL),
        "case {case:?}: expected the refusal to end {PLANE_TAIL:?}, got {actual:?}"
    );
    let id = &actual[head.len()..actual.len() - PLANE_TAIL.len()];
    assert!(
        is_uuid(id),
        "case {case:?}: the tail must carry the id of the call that was made, got {id:?}"
    );
    id.to_owned()
}

// ---------------------------------------------------------------------------
// RULE: every insight id this read cannot resolve is refused 404
// `ResourceNotFoundException` with the insights plane's own sentence — the id asked
// for and the id the cluster publishes for itself — followed by the tail naming
// that plane, the status it answered with, the id of that call and the attempt
// count, with a fresh call id per refusal.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unresolvable_insight_read_carries_the_wrapped_plane_refusal() {
    const CLUSTER: &str = "insight-plane";
    const NEIGHBOUR: &str = "insight-plane-neighbour";
    let router = emulator();

    // Three clusters, created before any wait so their activations run together: the
    // one the rows address, a second in the same region, and one of the SAME name in
    // another region.
    create_cluster(&router, REGION, CLUSTER).await;
    create_cluster(&router, REGION, NEIGHBOUR).await;
    create_cluster(&router, OTHER_REGION, CLUSTER).await;

    let cluster_id = published_cluster_id(&router, "setup", REGION, CLUSTER).await;
    let neighbour_id = published_cluster_id(&router, "setup", REGION, NEIGHBOUR).await;
    let elsewhere_id = published_cluster_id(&router, "setup", OTHER_REGION, CLUSTER).await;
    for (case, left, right) in [
        ("two clusters of one region", &cluster_id, &neighbour_id),
        ("one name in two regions", &cluster_id, &elsewhere_id),
    ] {
        assert_ne!(
            left, right,
            "setup {case:?}: each cluster publishes its own id, got {left:?} twice"
        );
    }

    // CONTROL: an id the cluster's own listing reports IS served, so every row below
    // is refused for the id it names and not because the read refuses everything.
    let listed = list_insights(&router, REGION, CLUSTER).await;
    assert_eq!(
        listed.status, 200,
        "control: expected 200 from ListInsights, got {listed:?}"
    );
    let served_id = listed.body["insights"]
        .as_array()
        .and_then(|insights| insights.first())
        .and_then(|insight| insight["id"].as_str())
        .unwrap_or_else(|| panic!("control: the cluster must report insights, got {listed:?}"))
        .to_owned();
    let served = describe_insight(&router, REGION, CLUSTER, &served_id).await;
    assert_eq!(
        served.status, 200,
        "control: an insight the listing reports is readable, got {served:?}"
    );
    assert!(
        !served.message().contains("Service:"),
        "control: a served read carries no relayed tail, got {served:?}"
    );

    // (case, region, cluster, the id the cluster publishes for itself, the id asked
    // for) — rows differ along the shape of the unresolvable id and along the
    // cluster the read is addressed to.
    let rows: Vec<(&str, &str, &str, &str, String)> = vec![
        (
            "a well-formed UUID no analysis minted",
            REGION,
            CLUSTER,
            &cluster_id,
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "a real insight id in upper case",
            REGION,
            CLUSTER,
            &cluster_id,
            served_id.to_uppercase(),
        ),
        (
            "a value that is not a UUID",
            REGION,
            CLUSTER,
            &cluster_id,
            "not-an-insight".to_string(),
        ),
        (
            "a value needing path encoding",
            REGION,
            CLUSTER,
            &cluster_id,
            "insight id/with slash".to_string(),
        ),
        (
            "a single character",
            REGION,
            CLUSTER,
            &cluster_id,
            "x".to_string(),
        ),
        (
            "a 256-character value",
            REGION,
            CLUSTER,
            &cluster_id,
            "7".repeat(256),
        ),
        (
            "an id of this cluster asked of its neighbour",
            REGION,
            NEIGHBOUR,
            &neighbour_id,
            served_id.clone(),
        ),
        (
            "the same id asked of the same-named cluster in another region",
            OTHER_REGION,
            CLUSTER,
            &elsewhere_id,
            served_id.clone(),
        ),
    ];

    let mut call_ids: BTreeSet<String> = BTreeSet::new();
    for (case, region, cluster, published_id, id) in &rows {
        let refused = describe_insight(&router, region, cluster, id).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected HTTP 404, got {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {refused:?}"
        );
        let actual = refused.message();
        let sentence = format!("Cannot find insight insightId={id} clusterId={published_id}");
        let call_id = relayed_request_id(case, &sentence, &actual);
        assert!(
            call_ids.insert(call_id.clone()),
            "case {case:?}: each call carries its own request id, but {call_id:?} appeared twice"
        );
        assert!(
            !actual.contains(cluster),
            "case {case:?}: the refusal reports the cluster's id, not the name the request \
             addressed it with, got {actual:?}"
        );
        // The payload names no resource of its own: the INSIGHT is what is missing,
        // and the exception models no member for one.
        assert!(
            refused
                .body
                .get("clusterName")
                .is_none_or(serde_json::Value::is_null),
            "case {case:?}: a missing insight names no cluster member, got {refused:?}"
        );
    }

    // CONTRAST: the Amazon EKS control plane's own refusal. A cluster the region does
    // not hold is answered by this service itself, so there is no relayed call to
    // name and no tail is appended.
    let case = "a cluster the region does not hold";
    let unknown = describe_insight(&router, REGION, "insight-plane-absent", "not-an-insight").await;
    assert_eq!(
        unknown.status, 404,
        "case {case:?}: expected HTTP 404, got {unknown:?}"
    );
    let message = unknown.message();
    assert!(
        !message.contains("Service:") && !message.contains("SDK Attempt Count"),
        "case {case:?}: a refusal this service composed itself names no relayed call, got \
         {message:?}"
    );
}
