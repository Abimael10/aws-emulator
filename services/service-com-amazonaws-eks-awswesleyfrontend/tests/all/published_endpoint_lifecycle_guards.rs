// SPDX-License-Identifier: CC0-1.0

//! The guards around a cluster's PUBLISHED address: what every projection of a
//! cluster hands a client to dial, what a delete the dependency guard refuses
//! leaves behind, and what a cluster that is not running yet answers a
//! configuration change with.
//!
//! Three rules, each over a TABLE whose rows differ along what it generalises over
//! rather than along one recorded request; every assertion names its case and
//! prints the actual value:
//!
//! * a value this service publishes for a client to DIAL is an address this
//!   emulator answers on. So EVERY projection of one cluster that carries
//!   `Cluster$endpoint` ("the endpoint for your Kubernetes API server") publishes
//!   the address that cluster's own listener answers on — never a name under AWS's
//!   own zone, which resolves to AWS production or to nothing on the machine the
//!   emulator runs on — the create's answer publishes none, because the control
//!   plane behind the cluster does not exist yet, and the DELETE's answer, the last
//!   projection a client reads, publishes the same address the describe did. The
//!   rows differ along the region and the form of the name.
//! * a cluster is deleted only once nothing is attached to it: "If you have managed
//!   node groups or Fargate profiles attached to the cluster, you must delete them
//!   first" (`DeleteCluster`). The call is refused `ResourceInUseException` (409)
//!   and the cluster is left exactly as it was — still `ACTIVE`, still publishing
//!   the same address, and that address still serving its Kubernetes API — and the
//!   same call is accepted once the children are gone. The rows differ along which
//!   child is attached and how many.
//! * a configuration change is applied to a running control plane, so
//!   `UpdateClusterConfig` on a cluster that is not `ACTIVE` is refused
//!   `InvalidRequestException` (400) with the service's own fixed sentence — ONE
//!   sentence for every change, naming neither the cluster nor the status it is in —
//!   and starts no update. The rows differ along the category of change asked for,
//!   including a change that is itself invalid, which the state refusal precedes.
//!
//! Nothing here waits out a span: the clock-owed `CREATING` -> `ACTIVE` move is
//! waited for by LOOKING AGAIN with a generous deadline (the shared
//! [`crate::kubernetes_api::poll_until_active`] and [`crate::wire::read_until`]),
//! and the clusters of a table are created before any of them is waited for, so
//! the waits overlap instead of adding up.
//!
//! The control-plane harness (`Answer`, `emulator`, `call`, `encode_label`) is the
//! shared one in `tests/all/wire.rs`; the client that dials the published address
//! is the raw kubeconfig-built one in `tests/all/kubernetes_api.rs`.

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_endpoint_publication::own_address;
use crate::cluster_lifecycle::{minimal_request, REGION};
use crate::kubernetes_api::{dial, kubeconfig_of, poll_until_active, Kubeconfig};
use crate::nodegroup_lifecycle::minimal_request as nodegroup_request;
use crate::wire::{call, emulator, encode_label, error_code, read_until, Answer};

/// AWS's own sentence for a configuration change asked of a cluster that is not
/// running, written out here rather than read from the crate's own constant so this
/// suite pins the WORDING a client sees instead of re-asserting whatever the handler
/// happens to carry.
const NOT_ACTIVE_FOR_UPDATE: &str =
    "Cluster is currently not in Active State, so it cannot be updated";

/// The sentence a delete of a cluster that still has a managed node group earns,
/// written out here for the same reason.
const NODEGROUPS_ATTACHED: &str = "Cluster has nodegroups attached";

/// The sentence a delete of a cluster that still has a Fargate profile earns.
const FARGATE_PROFILES_ATTACHED: &str = "Cluster has Fargate Profiles in ACTIVE/CREATING state";

/// `CreateCluster` in `region`, with the smallest body the operation accepts.
async fn create_cluster_in(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "POST",
        "/clusters",
        &minimal_request(name).to_string(),
    )
    .await
}

async fn describe_cluster_in(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "GET",
        &format!("/clusters/{}", encode_label(name)),
        "",
    )
    .await
}

async fn delete_cluster_in(router: &MultiServiceRouter, region: &str, name: &str) -> Answer {
    call(
        router,
        region,
        "DELETE",
        &format!("/clusters/{}", encode_label(name)),
        "",
    )
    .await
}

async fn update_config(
    router: &MultiServiceRouter,
    cluster: &str,
    body: &serde_json::Value,
) -> Answer {
    call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/update-config", encode_label(cluster)),
        &body.to_string(),
    )
    .await
}

/// `ListUpdates`, which is where "an update started" is read back from: a refused
/// change must leave this listing empty.
async fn list_updates(router: &MultiServiceRouter, cluster: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!("/clusters/{}/updates", encode_label(cluster)),
        "",
    )
    .await
}

async fn create_nodegroup(router: &MultiServiceRouter, cluster: &str, name: &str) -> Answer {
    call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/node-groups", encode_label(cluster)),
        &nodegroup_request(name).to_string(),
    )
    .await
}

fn nodegroup_path(cluster: &str, name: &str) -> String {
    format!(
        "/clusters/{}/node-groups/{}",
        encode_label(cluster),
        encode_label(name)
    )
}

/// `CreateFargateProfile`: a profile needs an execution role of the account this
/// emulator serves and at least one selector ("Fargate Profile must have at least
/// one selector").
async fn create_fargate_profile(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
) -> Answer {
    let body = serde_json::json!({
        "fargateProfileName": name,
        "podExecutionRoleArn": format!(
            "arn:aws:iam::{}:role/AmazonEKSFargatePodExecutionRole",
            engine::emulator_account_id()
        ),
        "subnets": ["subnet-aaaa1111", "subnet-bbbb2222"],
        "selectors": [{ "namespace": "default" }],
    });
    call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/fargate-profiles", encode_label(cluster)),
        &body.to_string(),
    )
    .await
}

async fn delete_fargate_profile(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
) -> Answer {
    call(
        router,
        REGION,
        "DELETE",
        &format!(
            "/clusters/{}/fargate-profiles/{}",
            encode_label(cluster),
            encode_label(name)
        ),
        "",
    )
    .await
}

/// The `endpoint` one answer publishes, as the string a client would dial.
fn published_endpoint(case: &str, answer: &Answer) -> String {
    answer.cluster()["endpoint"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a published endpoint, got {:?}",
                answer.cluster()
            )
        })
        .to_string()
}

/// Dial the published address the way a kubeconfig-built client does and assert the
/// cluster's own Kubernetes API answers there: `default`, the namespace Kubernetes
/// creates with every cluster, reads back as the object it is.
async fn assert_serves_its_api(config: &Kubeconfig, case: &str) {
    let served = dial(config, case, "GET", "/api/v1/namespaces/default", None).await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: the published address must serve this cluster's Kubernetes API, got \
         {served:?}"
    );
    assert_eq!(
        served.body["metadata"]["name"],
        serde_json::json!("default"),
        "case {case:?}: expected the cluster's own default namespace, got {:?}",
        served.body
    );
}

// ---------------------------------------------------------------------------
// RULE: every projection of a cluster that carries `endpoint` publishes the
// address this emulator answers THAT cluster's Kubernetes API on — and a cluster
// whose control plane does not exist yet publishes none.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_projection_that_carries_the_endpoint_publishes_an_address_this_emulator_serves() {
    let router = emulator();

    // The rows differ along what the rule generalises over: the region the cluster
    // lives in and the form of its name.
    let rows: [(&str, &str, &str); 3] = [
        ("a cluster in the suite's region", REGION, "guarded-endpoint-one"),
        ("the same name in another region", "eu-central-1", "guarded-endpoint-one"),
        ("a name spelled with underscores", REGION, "guarded_endpoint_two"),
    ];

    // Created first, waited for afterwards: the move to ACTIVE is one the clock owes
    // each cluster from its own create, so the waits overlap.
    for (case, region, name) in rows {
        let created = create_cluster_in(&router, region, name).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: setup CreateCluster expected 200, got {created:?}"
        );
        // "The API server endpoint and certificate authority data aren't available
        // until the cluster reaches the ACTIVE state": there is nothing to dial yet,
        // so nothing is published — never a placeholder a client could be pointed at.
        assert_eq!(
            created.cluster()["status"],
            serde_json::json!("CREATING"),
            "case {case:?}: expected a CREATING cluster, got {:?}",
            created.cluster()["status"]
        );
        assert!(
            created.cluster()["endpoint"].is_null(),
            "case {case:?}: a cluster with no control plane yet publishes no endpoint, got {:?}",
            created.cluster()["endpoint"]
        );
    }

    for (case, region, name) in rows {
        let described = poll_until_active(&router, region, name).await;
        let published = published_endpoint(case, &described);
        // An address of this emulator's own, with the port its listener answers on.
        let (_host, _port) = own_address(case, &published);
        // And what answers there is THIS cluster's API server, reached under the
        // certificate authority the same answer published.
        let config = kubeconfig_of(name, &described);
        assert_serves_its_api(&config, case).await;

        // The LAST projection a client reads: the delete answers with the cluster it
        // is taking down, and the address it publishes there is the one every other
        // read published.
        let deleted = delete_cluster_in(&router, region, name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected HTTP 200 from DeleteCluster, got {deleted:?}"
        );
        assert_eq!(
            deleted.cluster()["status"],
            serde_json::json!("DELETING"),
            "case {case:?}: the accepted delete reports DELETING, got {:?}",
            deleted.cluster()["status"]
        );
        assert_eq!(
            deleted.cluster()["endpoint"],
            serde_json::json!(published),
            "case {case:?}: the delete must publish the address the cluster was reached at \
             ({published:?}), got {:?}",
            deleted.cluster()["endpoint"]
        );
        let (_host, _port) = own_address(case, &published_endpoint(case, &deleted));
    }
}

// ---------------------------------------------------------------------------
// RULE: a delete the dependency guard refuses leaves the cluster as it was —
// `ACTIVE`, publishing the same address, and that address still serving its
// Kubernetes API — and the same call is accepted once nothing is attached.
// ---------------------------------------------------------------------------

/// One row of the dependency table: what is attached to the cluster, and the
/// sentence the refusal that keeps it carries.
struct Attached {
    case: &'static str,
    cluster: &'static str,
    nodegroups: &'static [&'static str],
    profiles: &'static [&'static str],
    refusal: &'static str,
}

#[tokio::test]
async fn a_refused_delete_leaves_the_cluster_serving_the_address_it_published() {
    let router = emulator();

    // The rows differ along WHICH child holds the cluster and HOW MANY of them: one
    // managed node group, two of them, and a Fargate profile — the two kinds the
    // documented sentence names, each with its own wording.
    let rows: [Attached; 3] = [
        Attached {
            case: "one managed node group",
            cluster: "guarded-delete-one",
            nodegroups: &["ng-a"],
            profiles: &[],
            refusal: NODEGROUPS_ATTACHED,
        },
        Attached {
            case: "two managed node groups",
            cluster: "guarded-delete-two",
            nodegroups: &["ng-a", "ng-b"],
            profiles: &[],
            refusal: NODEGROUPS_ATTACHED,
        },
        Attached {
            case: "one Fargate profile",
            cluster: "guarded-delete-three",
            nodegroups: &[],
            profiles: &["fp-a"],
            refusal: FARGATE_PROFILES_ATTACHED,
        },
    ];

    for row in &rows {
        let created = create_cluster_in(&router, REGION, row.cluster).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: setup CreateCluster expected 200, got {created:?}",
            row.case
        );
    }

    for row in &rows {
        let case = row.case;
        let described = poll_until_active(&router, REGION, row.cluster).await;
        let published = published_endpoint(case, &described);
        let config = kubeconfig_of(row.cluster, &described);

        for name in row.nodegroups {
            let answer = create_nodegroup(&router, row.cluster, name).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: setup: the node group {name:?} must be created, got {answer:?}"
            );
        }
        for name in row.profiles {
            let answer = create_fargate_profile(&router, row.cluster, name).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: setup: the profile {name:?} must be created, got {answer:?}"
            );
        }

        let refused = delete_cluster_in(&router, REGION, row.cluster).await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: expected HTTP 409 while a child is attached, got {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceInUseException",
            "case {case:?}: expected ResourceInUseException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            row.refusal,
            "case {case:?}: expected message {:?}, got {:?}",
            row.refusal,
            refused.message()
        );

        // The refusal tore nothing down: the cluster is still running, still
        // publishes the same address, and that address still answers its Kubernetes
        // API — which is what distinguishes a dependency guard from a delete that
        // went ahead.
        let still = describe_cluster_in(&router, REGION, row.cluster).await;
        assert_eq!(
            still.status, 200,
            "case {case:?}: a refused delete must keep the cluster, got {still:?}"
        );
        assert_eq!(
            still.cluster()["status"],
            serde_json::json!("ACTIVE"),
            "case {case:?}: a refused delete must leave the status untouched, got {:?}",
            still.cluster()["status"]
        );
        assert_eq!(
            still.cluster()["endpoint"],
            serde_json::json!(published),
            "case {case:?}: a refused delete must leave the published address untouched \
             ({published:?}), got {:?}",
            still.cluster()["endpoint"]
        );
        assert_serves_its_api(&config, case).await;

        // And the same call is accepted once nothing is attached. A node group's
        // removal is ASYNCHRONOUS — it is still a node group of the cluster until the
        // clock has finished it — so that is waited for by LOOKING AGAIN.
        for name in row.nodegroups {
            let removed = call(
                &router,
                REGION,
                "DELETE",
                &nodegroup_path(row.cluster, name),
                "",
            )
            .await;
            assert_eq!(
                removed.status, 200,
                "case {case:?}: the node group {name:?} must delete, got {removed:?}"
            );
        }
        for name in row.profiles {
            let removed = delete_fargate_profile(&router, row.cluster, name).await;
            assert_eq!(
                removed.status, 200,
                "case {case:?}: the profile {name:?} must delete, got {removed:?}"
            );
        }
        for name in row.nodegroups {
            let path = nodegroup_path(row.cluster, name);
            read_until(
                case,
                &format!("the removal of the node group {name:?} finished"),
                || call(&router, REGION, "GET", &path, ""),
                |read| read.status == 404,
            )
            .await;
        }

        let deleted = delete_cluster_in(&router, REGION, row.cluster).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected HTTP 200 once nothing is attached, got {deleted:?}"
        );
        assert_eq!(
            deleted.cluster()["status"],
            serde_json::json!("DELETING"),
            "case {case:?}: the accepted delete reports DELETING, got {:?}",
            deleted.cluster()["status"]
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: `UpdateClusterConfig` on a cluster that is not ACTIVE is refused 400
// `InvalidRequestException` in ONE fixed sentence, which names neither the cluster
// nor the status it is in, and starts no update.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cluster_that_is_not_running_takes_no_configuration_change() {
    let router = emulator();

    // The rows differ along the CATEGORY of change asked for — the kinds
    // `UpdateClusterConfig` takes (`smithy_query query="operation"
    // operation="UpdateClusterConfig"`: `logging`, `resourcesVpcConfig`,
    // `accessConfig`, `deletionProtection`) — plus a change that is itself invalid,
    // which the state refusal precedes. Each row gets its OWN cluster and is
    // addressed straight after the create that starts it, which is the window the
    // status it reports is in.
    let rows: [(&str, serde_json::Value); 5] = [
        (
            "a control-plane logging change",
            serde_json::json!({
                "logging": { "clusterLogging": [{ "types": ["api"], "enabled": true }] },
            }),
        ),
        (
            "an endpoint-access change",
            serde_json::json!({
                "resourcesVpcConfig": { "endpointPrivateAccess": true },
            }),
        ),
        (
            "an access-configuration change",
            serde_json::json!({ "accessConfig": { "authenticationMode": "API_AND_CONFIG_MAP" } }),
        ),
        (
            "the deletion-protection switch",
            serde_json::json!({ "deletionProtection": true }),
        ),
        (
            // Both endpoint paths off is a configuration Amazon EKS has no cluster
            // for, so this body would be refused on its own merits once the cluster
            // were running: the STATE refusal comes first.
            "a change that is itself invalid",
            serde_json::json!({
                "resourcesVpcConfig": {
                    "endpointPublicAccess": false,
                    "endpointPrivateAccess": false,
                },
            }),
        ),
    ];

    for (index, (case, body)) in rows.iter().enumerate() {
        let cluster = format!("guarded-update-{index}");
        let created = create_cluster_in(&router, REGION, &cluster).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: setup CreateCluster expected 200, got {created:?}"
        );
        assert_eq!(
            created.cluster()["status"],
            serde_json::json!("CREATING"),
            "case {case:?}: setup: the row needs a cluster that is not running yet, got {:?}",
            created.cluster()["status"]
        );

        let refused = update_config(&router, &cluster, body).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            NOT_ACTIVE_FOR_UPDATE,
            "case {case:?}: expected message {NOT_ACTIVE_FOR_UPDATE:?}, got {:?}",
            refused.message()
        );
        // The sentence is the service's own fixed text: it carries neither the
        // cluster the request addressed nor the status the cluster is in.
        assert!(
            !refused.message().contains(&cluster),
            "case {case:?}: the refusal must not name the cluster, got {:?}",
            refused.message()
        );
        assert!(
            !refused.message().contains("CREATING"),
            "case {case:?}: the refusal must not name the status, got {:?}",
            refused.message()
        );

        // And it started nothing: there is no update to read back.
        let listed = list_updates(&router, &cluster).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected HTTP 200 from ListUpdates, got {listed:?}"
        );
        assert_eq!(
            listed.body["updateIds"],
            serde_json::json!([]),
            "case {case:?}: a refused change starts no update, got {:?}",
            listed.body["updateIds"]
        );
    }

    // CONTROL: the same change on a cluster that IS running is served, so the rows
    // above are refused for the state the cluster is in and not because the
    // operation refuses everything.
    let cluster = "guarded-update-control";
    let created = create_cluster_in(&router, REGION, cluster).await;
    assert_eq!(
        created.status, 200,
        "control: setup CreateCluster expected 200, got {created:?}"
    );
    poll_until_active(&router, REGION, cluster).await;
    let accepted = update_config(&router, cluster, &rows[0].1).await;
    assert_eq!(
        accepted.status, 200,
        "control: the same change on an ACTIVE cluster is served, got {accepted:?}"
    );
    assert!(
        accepted.body["update"]["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "control: the accepted change reports the update it started, got {:?}",
        accepted.body
    );
}
