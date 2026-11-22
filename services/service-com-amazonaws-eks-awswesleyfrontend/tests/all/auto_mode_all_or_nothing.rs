// SPDX-License-Identifier: CC0-1.0

//! The sentence a partial EKS Auto Mode configuration earns, driven end to end
//! through the same `MultiServiceRouter` the production binary mounts.
//!
//! RULE: EKS Auto Mode is ONE mode of the cluster made of three capabilities —
//! compute (`CreateClusterRequest$computeConfig`), block storage
//! (`$storageConfig.blockStorage`) and elastic load balancing
//! (`$kubernetesNetworkConfig.elasticLoadBalancing`) — so a request that would
//! leave them in disagreement is refused `InvalidParameterException` (HTTP 400,
//! from the model) with the ONE sentence Amazon EKS states for that
//! misconfiguration, whichever of the three is out of step and whichever operation
//! asked; a request that leaves all three on, or all three off, is served. The
//! compute capability's own relation is worded the same way: a `nodePools` list
//! without the `nodeRoleArn` its instances run as earns that relation's one
//! sentence.
//!
//! Why it matters: the message is the only machine-readable discriminator between
//! the several `InvalidParameterException` causes these operations raise, so a
//! client, a runbook or an operator that matches on this sentence has to read the
//! sentence the real service answers.
//!
//! The table spans what the rule generalises over: every combination of the three
//! capabilities over the three states a request can leave each one in — not named at
//! all, named `false`, named `true` — which is the whole class the all-or-nothing
//! rule divides, rather than one recorded body. The update half spans the same
//! disagreements reached by CHANGING a cluster that has the mode fully off.
//!
//! The wire harness (`Answer`, `emulator`, `call`, `encode_label`) is the shared one
//! in `tests/all/wire.rs` and the `CreateCluster` body is `cluster_lifecycle`'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{minimal_request, REGION};
use crate::wire::{call, emulator, encode_label, error_code, read_until, Answer};

/// The sentence a partial EKS Auto Mode configuration earns, written out here
/// rather than read from the crate's own constant so this suite pins the WORDING a
/// client sees. Entirely fixed text: it names the three configurations the mode is
/// made of and carries nothing from the request.
const AUTO_MODE_ALL_OR_NOTHING: &str =
    "For EKS Auto Mode, please ensure that all required configs, including computeConfig, \
     kubernetesNetworkConfig, and blockStorage are all either fully enabled or fully disabled.";

/// The sentence a compute capability that names node pools but no node role earns,
/// written out here for the same reason. Entirely fixed text too: it names the two
/// members of the relation the request broke.
const NODE_POOLS_NEED_A_NODE_ROLE: &str =
    "When Compute Config nodeRoleArn is null or empty, nodePool list cannot be populated.";

/// The three states a request can leave one capability in: not named at all, named
/// `false`, named `true`.
const STATES: [Option<bool>; 3] = [None, Some(false), Some(true)];

/// Whether a capability a request did not name is one it enabled: a capability the
/// request is silent about is not enabled.
fn effective(state: Option<bool>) -> bool {
    state.unwrap_or(false)
}

/// The `CreateCluster` body that leaves the three capabilities in the named states.
fn request(
    name: &str,
    compute: Option<bool>,
    block_storage: Option<bool>,
    load_balancing: Option<bool>,
) -> serde_json::Value {
    let mut body = minimal_request(name);
    if let Some(enabled) = compute {
        body["computeConfig"] = serde_json::json!({ "enabled": enabled });
    }
    if let Some(enabled) = block_storage {
        body["storageConfig"] = serde_json::json!({ "blockStorage": { "enabled": enabled } });
    }
    if let Some(enabled) = load_balancing {
        body["kubernetesNetworkConfig"] =
            serde_json::json!({ "elasticLoadBalancing": { "enabled": enabled } });
    }
    body
}

async fn create_cluster(router: &MultiServiceRouter, body: &serde_json::Value) -> Answer {
    call(router, REGION, "POST", "/clusters", &body.to_string()).await
}

async fn describe_cluster(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!("/clusters/{}", encode_label(name)),
        "",
    )
    .await
}

async fn update_config(
    router: &MultiServiceRouter,
    name: &str,
    change: &serde_json::Value,
) -> Answer {
    call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/update-config", encode_label(name)),
        &change.to_string(),
    )
    .await
}

// ---------------------------------------------------------------------------
// RULE: a CREATE that leaves the three EKS Auto Mode capabilities in disagreement
// is refused with that one sentence and writes nothing; one that leaves them in
// agreement is served.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_partial_auto_mode_create_is_refused_with_the_one_sentence() {
    let router = emulator();

    let mut index = 0;
    for compute in STATES {
        for block_storage in STATES {
            for load_balancing in STATES {
                let case = format!(
                    "computeConfig.enabled {compute:?}, storageConfig.blockStorage.enabled \
                     {block_storage:?}, kubernetesNetworkConfig.elasticLoadBalancing.enabled \
                     {load_balancing:?}"
                );
                let name = format!("auto-mode-{index}");
                index += 1;
                let body = request(&name, compute, block_storage, load_balancing);
                let answer = create_cluster(&router, &body).await;

                let agree = effective(compute) == effective(block_storage)
                    && effective(compute) == effective(load_balancing);
                if agree {
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?}: a configuration the mode has is served, got {answer:?}"
                    );
                    let described = describe_cluster(&router, &name).await;
                    assert_eq!(
                        described.status, 200,
                        "case {case:?}: the created cluster must read back, got {described:?}"
                    );
                    continue;
                }
                assert_eq!(
                    (answer.status, error_code(&answer).as_str()),
                    (400, "InvalidParameterException"),
                    "case {case:?}: expected 400 InvalidParameterException, got {answer:?}"
                );
                assert_eq!(
                    answer.message(),
                    AUTO_MODE_ALL_OR_NOTHING,
                    "case {case:?}: expected the all-or-nothing sentence, got {:?}",
                    answer.message()
                );
                // A refused create writes nothing, so the name holds no cluster.
                let described = describe_cluster(&router, &name).await;
                assert_eq!(
                    described.status, 404,
                    "case {case:?}: a refused create must write no cluster, got {described:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: the compute capability's node pools name the role their instances run as,
// so a pool list without one earns that relation's own sentence.
// ---------------------------------------------------------------------------

/// The rows differ along how many pools the list carries and which pools they are;
/// the mirror rows name the role, or no pool at all, which is served.
///
/// A role that is NAMED but empty is not a row here: naming a role at all puts the
/// request through the `iam:PassRole` decision, which is made before the body is
/// judged (`authorization_before_the_body`), so such a request never reaches this
/// relation.
#[tokio::test]
async fn node_pools_without_a_node_role_are_refused_with_the_one_sentence() {
    let router = emulator();

    // (case, the `computeConfig` members beside `enabled`, whether it is refused)
    let rows = [
        (
            "one pool and no role at all",
            serde_json::json!({ "nodePools": ["general-purpose"] }),
            true,
        ),
        (
            "two pools and no role at all",
            serde_json::json!({ "nodePools": ["general-purpose", "system"] }),
            true,
        ),
        (
            "a single pool the general-purpose row does not name",
            serde_json::json!({ "nodePools": ["system"] }),
            true,
        ),
        (
            "a pool and the role its instances run as",
            serde_json::json!({
                "nodePools": ["general-purpose"],
                "nodeRoleArn": "arn:aws:iam::000000000000:role/eksNodeRole",
            }),
            false,
        ),
        (
            "no pool list at all, so there is nothing to run",
            serde_json::json!({}),
            false,
        ),
    ];

    for (index, (case, compute, refused)) in rows.iter().enumerate() {
        let name = format!("node-pools-{index}");
        // The three capabilities agree, so the configuration is judged on the node
        // pools rather than on the mode being partial.
        let mut body = request(&name, Some(true), Some(true), Some(true));
        let mut compute_config = body["computeConfig"].clone();
        for (member, value) in compute.as_object().expect("a row's members are an object") {
            compute_config[member.as_str()] = value.clone();
        }
        body["computeConfig"] = compute_config;
        let answer = create_cluster(&router, &body).await;

        if !*refused {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: a compute capability EKS serves is created, got {answer:?}"
            );
            continue;
        }
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, "InvalidParameterException"),
            "case {case:?}: expected 400 InvalidParameterException, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            NODE_POOLS_NEED_A_NODE_ROLE,
            "case {case:?}: expected the node-role sentence, got {:?}",
            answer.message()
        );
        let described = describe_cluster(&router, &name).await;
        assert_eq!(
            described.status, 404,
            "case {case:?}: a refused create must write no cluster, got {described:?}"
        );
    }
}


/// The rows are the ways a change can leave the three capabilities in
/// disagreement — each one alone, each pair without the third — plus the change
/// that enables all three together, which is served.
#[tokio::test]
async fn a_change_that_would_leave_auto_mode_partial_is_refused_with_the_one_sentence() {
    let router = emulator();

    // (case, the change, whether it leaves the three in agreement)
    let rows = [
        (
            "compute alone",
            serde_json::json!({ "computeConfig": { "enabled": true } }),
            false,
        ),
        (
            "block storage alone",
            serde_json::json!({ "storageConfig": { "blockStorage": { "enabled": true } } }),
            false,
        ),
        (
            "load balancing alone",
            serde_json::json!({
                "kubernetesNetworkConfig": { "elasticLoadBalancing": { "enabled": true } },
            }),
            false,
        ),
        (
            "compute and block storage without load balancing",
            serde_json::json!({
                "computeConfig": { "enabled": true },
                "storageConfig": { "blockStorage": { "enabled": true } },
            }),
            false,
        ),
        (
            "compute and load balancing without block storage",
            serde_json::json!({
                "computeConfig": { "enabled": true },
                "kubernetesNetworkConfig": { "elasticLoadBalancing": { "enabled": true } },
            }),
            false,
        ),
        (
            "block storage and load balancing without compute",
            serde_json::json!({
                "storageConfig": { "blockStorage": { "enabled": true } },
                "kubernetesNetworkConfig": { "elasticLoadBalancing": { "enabled": true } },
            }),
            false,
        ),
        (
            "all three together",
            serde_json::json!({
                "computeConfig": { "enabled": true },
                "storageConfig": { "blockStorage": { "enabled": true } },
                "kubernetesNetworkConfig": { "elasticLoadBalancing": { "enabled": true } },
            }),
            true,
        ),
    ];

    // Every row's cluster starts with the mode fully off, so the configuration the
    // change leaves is the change's own.
    for (index, (case, _, _)) in rows.iter().enumerate() {
        let name = format!("auto-mode-change-{index}");
        let created = create_cluster(&router, &request(&name, Some(false), Some(false), Some(false)))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: setup: the cluster must be created, got {created:?}"
        );
    }
    // A change is only started on a cluster that is running, which is waited for by
    // LOOKING AGAIN rather than by waiting out the activation span
    // ([`crate::wire::read_until`]).
    for (index, (case, _, _)) in rows.iter().enumerate() {
        let name = format!("auto-mode-change-{index}");
        read_until(
            case,
            "the cluster reported ACTIVE",
            || describe_cluster(&router, &name),
            |read| read.cluster()["status"] == serde_json::json!("ACTIVE"),
        )
        .await;
    }

    for (index, (case, change, agree)) in rows.iter().enumerate() {
        let name = format!("auto-mode-change-{index}");
        let answer = update_config(&router, &name, change).await;
        if *agree {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: a change into a configuration the mode has is served, got \
                 {answer:?}"
            );
            assert!(
                answer.body["update"]["id"].is_string(),
                "case {case:?}: an accepted change starts one update, got {answer:?}"
            );
            continue;
        }
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, "InvalidParameterException"),
            "case {case:?}: expected 400 InvalidParameterException, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            AUTO_MODE_ALL_OR_NOTHING,
            "case {case:?}: expected the all-or-nothing sentence, got {:?}",
            answer.message()
        );
        // A refused change starts no update.
        let updates = call(
            &router,
            REGION,
            "GET",
            &format!("/clusters/{}/updates", encode_label(&name)),
            "",
        )
        .await;
        assert_eq!(
            updates.body["updateIds"],
            serde_json::json!([]),
            "case {case:?}: a refused change must record no update, got {updates:?}"
        );
    }
}
