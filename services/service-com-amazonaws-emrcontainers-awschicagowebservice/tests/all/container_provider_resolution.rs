// SPDX-License-Identifier: CC0-1.0

//! The CONTAINER CLUSTER a `CreateVirtualCluster` registers against: resolved against the
//! service that owns it before anything is registered.
//!
//! Every call goes through the production path: this service is mounted in the real
//! `MultiServiceRouter` by the harness [`crate::virtual_cluster_job_run_loop::Emr`], with
//! Amazon EKS and Amazon EC2 mounted BESIDE it on the one shared engine
//! ([`Emr::with_container_clusters`]) exactly as the assembled emulator mounts them, and
//! driven with restJson1 HTTP requests. Every container cluster a row names was created
//! through Amazon EKS's OWN API over the same in-process callback this service resolves a
//! provider through, so no identifier asserted here was written by this suite.
//!
//! The rule under test, asserted over a TABLE of providers rather than one request:
//! `containerProvider.id` is "The ID of the container cluster" — a resource of another
//! service — so a registration naming an id that service holds no cluster for registers
//! nothing and is refused with the modelled `ValidationException` (HTTP 400), worded around
//! the id the request named; an id it does hold is registered and reads back.
//!
//! The rows differ along what the rule generalises over: whether the owner holds the cluster
//! at all, how the named id differs from one it holds (a suffix, a prefix, its case), the
//! REGION the question is asked in, and whether the cluster was deleted after being created.

use serde_json::{json, Value};
use service_com_amazonaws_emrcontainers_awschicagowebservice::state;

use crate::managed_endpoint_connection::{create_container_cluster, delete_container_cluster};
use crate::virtual_cluster_job_run_loop::{field, text, Answer, Emr, REGION};

/// A Region whose endpoint holds its own clusters: a container cluster created in [`REGION`]
/// is not one this Region's Amazon EKS endpoint holds.
const OTHER_REGION: &str = "eu-central-1";

/// `POST /virtualclusters` naming `container_cluster` as the provider, signed for `region`.
async fn register(
    emr: &Emr,
    region: &str,
    name: &str,
    token: &str,
    container_cluster: &str,
) -> Answer {
    emr.send_in(
        region,
        "POST",
        "/virtualclusters",
        Some(json!({
            "name": name,
            "clientToken": token,
            "containerProvider": {"id": container_cluster, "type": "EKS"},
        })),
    )
    .await
}

/// The ids `ListVirtualClusters` reports for the clusters registered against
/// `container_cluster` at `region`'s endpoint.
async fn registered_against(
    emr: &Emr,
    region: &str,
    container_cluster: &str,
    case: &str,
) -> Vec<String> {
    let answer = emr
        .send_in(
            region,
            "GET",
            &format!("/virtualclusters?containerProviderId={container_cluster}"),
            None,
        )
        .await;
    let body = answer.ok(case).clone();
    field(&body, "/virtualClusters", case)
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: virtualClusters is not a list: {body}"))
        .iter()
        .map(|cluster| text(cluster, "/id", case))
        .collect()
}

/// The `message` a refused call carries.
fn refusal_message(body: &Value, case: &str) -> String {
    text(body, "/message", case)
}

/// AWS's sentence for a container cluster its owner holds nothing for: fixed text around the
/// id the request named.
fn not_found_sentence(container_cluster: &str) -> String {
    format!("Cluster {container_cluster} not found")
}

/// THE RULE: an id the owner of container clusters holds nothing for registers nothing.
#[tokio::test]
async fn a_container_provider_the_owner_does_not_hold_registers_nothing() {
    let emr = Emr::with_container_clusters();
    // Two container clusters that really exist, so a row can differ from a held id rather
    // than only from nothing at all. `create_container_cluster` creates them through Amazon
    // EKS's own API, in subnets Amazon EC2 reports for the account.
    create_container_cluster(&emr, "eks-held-one", 2).await;
    create_container_cluster(&emr, "eks-held-two", 2).await;

    // Every id here satisfies `ClusterId` (len 1..100, pattern
    // ^[0-9A-Za-z][A-Za-z0-9\-_]*$: smithy_query, operation `CreateVirtualCluster`), so each
    // one reaches the handler and is refused for naming no cluster rather than by the
    // constraint walk.
    let rows: Vec<(&str, String)> = vec![
        ("an id nothing was ever created under", "eks-absent".to_string()),
        (
            "a held cluster's name with a suffix",
            "eks-held-one-also".to_string(),
        ),
        ("a prefix of a held cluster's name", "eks-held".to_string()),
        (
            "a held cluster's name in upper case",
            "EKS-HELD-ONE".to_string(),
        ),
        (
            "a held cluster's name with an underscore for a hyphen",
            "eks_held_one".to_string(),
        ),
        ("the shortest id the model allows", "a".to_string()),
        ("the longest id the model allows", "z".repeat(100)),
        ("an all-digit id", "0123456789".to_string()),
    ];

    for (index, (label, container_cluster)) in rows.iter().enumerate() {
        let answer = register(
            &emr,
            REGION,
            &format!("unheld-{index}"),
            &format!("tok-unheld-{index}"),
            container_cluster,
        )
        .await;
        answer.refused(label, 400, "ValidationException");
        let message = refusal_message(&answer.body, label);
        assert_eq!(
            message,
            not_found_sentence(container_cluster),
            "case {label:?}: expected the sentence a container cluster nobody holds carries, \
             got {message:?}"
        );
        // Nothing was registered: no virtual cluster names that provider.
        let registered = registered_against(&emr, REGION, container_cluster, label).await;
        assert!(
            registered.is_empty(),
            "case {label:?}: a refused registration must leave no virtual cluster against \
             {container_cluster:?}, got {registered:?}"
        );
    }
}

/// THE RULE's other half: an id the owner DOES hold is registered, reads back `RUNNING` and
/// is the cluster the provider filter reports.
///
/// The rows are the container clusters the owner holds and how many virtual clusters are
/// registered against each, because one container cluster carries any number of them.
#[tokio::test]
async fn a_container_provider_the_owner_holds_is_registered_and_reads_back() {
    let emr = Emr::with_container_clusters();
    for (container_cluster, subnets, count) in [("eks-accepted-one", 2_usize, 1_usize), ("eks-accepted-two", 3, 2)] {
        create_container_cluster(&emr, container_cluster, subnets).await;

        let mut registered: Vec<String> = Vec::new();
        for nth in 0..count {
            let label = format!("{container_cluster}, registration {nth}");
            let answer = register(
                &emr,
                REGION,
                &format!("{container_cluster}-{nth}"),
                &format!("tok-{container_cluster}-{nth}"),
                container_cluster,
            )
            .await;
            let id = text(answer.ok(&label), "/id", &label);

            let described = emr.describe_cluster(&id).await;
            let cluster = field(described.ok(&label), "/virtualCluster", &label).clone();
            assert_eq!(
                text(&cluster, "/state", &label),
                state::CLUSTER_RUNNING,
                "case {label:?}: a registered virtual cluster is RUNNING: {cluster}"
            );
            assert_eq!(
                field(&cluster, "/containerProvider/id", &label),
                &json!(container_cluster),
                "case {label:?}: the record must name the container cluster it was \
                 registered against: {cluster}"
            );
            registered.push(id);
        }

        let label = format!("the clusters registered against {container_cluster}");
        let mut reported = registered_against(&emr, REGION, container_cluster, &label).await;
        reported.sort();
        let mut expected = registered.clone();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {label:?}: the provider filter must report exactly the registrations"
        );
    }
}

/// The question is asked in the REGION the request is signed for, so a container cluster of
/// one Region's Amazon EKS endpoint is not one another Region's endpoint holds.
#[tokio::test]
async fn a_container_provider_is_resolved_in_the_region_the_request_is_signed_for() {
    let emr = Emr::with_container_clusters();
    let container_cluster = "eks-regional";
    create_container_cluster(&emr, container_cluster, 2).await;

    // In the Region the cluster was created in, the registration is accepted.
    let case = "the Region the container cluster was created in";
    let answer = register(&emr, REGION, "regional-here", "tok-regional-here", container_cluster).await;
    let id = text(answer.ok(case), "/id", case);

    // At another Region's endpoint the same id names no container cluster.
    let case = "another Region's endpoint";
    let answer = register(
        &emr,
        OTHER_REGION,
        "regional-there",
        "tok-regional-there",
        container_cluster,
    )
    .await;
    answer.refused(case, 400, "ValidationException");
    assert_eq!(
        refusal_message(&answer.body, case),
        not_found_sentence(container_cluster),
        "case {case:?}: the refusal must name the container cluster the request asked for"
    );
    let registered = registered_against(&emr, OTHER_REGION, container_cluster, case).await;
    assert!(
        registered.is_empty(),
        "case {case:?}: no virtual cluster may be registered at that endpoint, got \
         {registered:?}"
    );

    // And the one that WAS registered is still there, in its own Region.
    let case = "the registration in the Region that holds the container cluster";
    let registered = registered_against(&emr, REGION, container_cluster, case).await;
    assert_eq!(
        registered,
        vec![id],
        "case {case:?}: the accepted registration must still be reported"
    );
}

/// The resolution follows the OWNER's state rather than a copy of it: once the container
/// cluster has been deleted through Amazon EKS's own API, a registration naming it is
/// refused.
#[tokio::test]
async fn a_container_provider_deleted_by_its_owner_registers_nothing() {
    let emr = Emr::with_container_clusters();
    let container_cluster = "eks-transient";
    create_container_cluster(&emr, container_cluster, 2).await;

    let case = "while Amazon EKS holds the container cluster";
    let answer = register(&emr, REGION, "transient-before", "tok-transient-before", container_cluster).await;
    let before = text(answer.ok(case), "/id", case);

    // `DeleteCluster` ACCEPTS the deletion: the owner keeps answering for the cluster in
    // its `DELETING` status until the deletion finishes, so the row below — which is
    // about a container cluster the owner holds NOTHING for — waits for that.
    delete_container_cluster(&emr, container_cluster).await;

    let case = "once Amazon EKS has deleted the container cluster";
    let answer = register(&emr, REGION, "transient-after", "tok-transient-after", container_cluster).await;
    answer.refused(case, 400, "ValidationException");
    assert_eq!(
        refusal_message(&answer.body, case),
        not_found_sentence(container_cluster),
        "case {case:?}: the refusal must name the container cluster the request asked for"
    );

    // The registration made while the container cluster existed is untouched.
    let registered = registered_against(&emr, REGION, container_cluster, case).await;
    assert_eq!(
        registered,
        vec![before],
        "case {case:?}: the earlier registration must still be reported, got {registered:?}"
    );
}
