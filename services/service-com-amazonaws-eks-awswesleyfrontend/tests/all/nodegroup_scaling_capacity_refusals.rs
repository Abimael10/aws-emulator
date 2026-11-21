// SPDX-License-Identifier: CC0-1.0

//! `UpdateNodegroupConfig` words a `scalingConfig` refusal the way Amazon EKS does:
//! it names the CAPACITY the change asked the node group to run at and the bound it
//! falls outside of, with the capacities the change would leave the node group with.
//!
//! The RULE under test: "the Cluster Autoscaler doesn't scale a managed node group
//! lower than `minSize` or higher than `maxSize`"
//! (`NodegroupScalingConfig$desiredSize`), so a change whose desired size sits
//! outside either bound is refused `InvalidParameterException` (400) — as
//! `Minimum capacity <minSize> can't be greater than desired size <desiredSize>`
//! when it is below the lower bound, and as
//! `desired capacity <desiredSize> can't be greater than max size <maxSize>` when
//! it is above the upper one. The two numbers in each sentence are the capacities
//! the node group would carry after the change, which is the merged triple:
//! "when updating a node group, you can specify any or none of the properties"
//! (`NodegroupScalingConfig`), so a member the request omits keeps its stored value.
//!
//! The rows differ along what the rule generalises over: which bound is violated,
//! which members the request carries (all three, or one that is judged against two
//! stored ones), the magnitude of the capacities, and the boundary cases where the
//! desired size sits exactly on a bound — which are accepted, and are the control
//! that the refusal is about the relation and not about the member being present.
//!
//! The wire harness (`Answer`, `emulator`, `call`, `encode_label`) is the shared one
//! in `tests/all/wire.rs`; the cluster and node group setup is
//! `nodegroup_lifecycle`'s, so this suite grows no second copy of either.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::REGION;
use crate::nodegroup_lifecycle::{create_cluster, create_nodegroup_in, minimal_request};
use crate::wire::{call, emulator, encode_label, error_code, Answer};

/// The capacities every node group in this suite starts from, so a row that names
/// one member is judged against two values it did not send.
const STORED_MIN: i64 = 1;
const STORED_MAX: i64 = 4;
const STORED_DESIRED: i64 = 2;

async fn update_config(
    router: &MultiServiceRouter,
    cluster: &str,
    nodegroup: &str,
    body: &serde_json::Value,
) -> Answer {
    call(
        router,
        REGION,
        "POST",
        &format!(
            "/clusters/{}/node-groups/{}/update-config",
            encode_label(cluster),
            encode_label(nodegroup)
        ),
        &body.to_string(),
    )
    .await
}

async fn describe_nodegroup(router: &MultiServiceRouter, cluster: &str, name: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!(
            "/clusters/{}/node-groups/{}",
            encode_label(cluster),
            encode_label(name)
        ),
        "",
    )
    .await
}

/// How long a polled read is given before the setup is declared broken. Generous
/// on purpose: the span a node group settles over is one the service chose in
/// milliseconds, so a deadline in seconds leaves a loaded host all the room it
/// needs without ever being the thing a case measures.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often that read is repeated: the loop ends on the first read that holds.
const INTERVAL: Duration = Duration::from_millis(25);

/// Stand up one `ACTIVE` node group named `ng` on a cluster of its own, carrying
/// the capacities above.
async fn active_nodegroup(router: &MultiServiceRouter, cluster: &str) {
    create_cluster(router, cluster).await;
    let mut body = minimal_request("ng");
    body["scalingConfig"] = serde_json::json!({
        "minSize": STORED_MIN,
        "maxSize": STORED_MAX,
        "desiredSize": STORED_DESIRED,
    });
    let created = create_nodegroup_in(router, REGION, cluster, &body).await;
    assert_eq!(
        created.status, 200,
        "setup: expected 200 from CreateNodegroup on {cluster:?}, got {created:?}"
    );
    // Waited out by polling for the CONDITION the rows need — an `ACTIVE` node
    // group — rather than for a span.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = describe_nodegroup(router, cluster, "ng").await;
        if described.body["nodegroup"]["status"] == serde_json::json!("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the node group on {cluster:?} never reached ACTIVE; the last answer was \
             {described:?}"
        );
        tokio::time::sleep(INTERVAL).await;
    }
}

/// One row: the `scalingConfig` a change carries, and the sentence it earns —
/// `None` for a change that must be accepted.
struct Case {
    case: &'static str,
    scaling: serde_json::Value,
    refusal: Option<String>,
}

fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "all three capacities, the minimum above the desired size",
            scaling: serde_json::json!({ "minSize": 5, "maxSize": 4, "desiredSize": 1 }),
            refusal: Some("Minimum capacity 5 can't be greater than desired size 1".to_string()),
        },
        Case {
            case: "all three capacities, the desired size above the maximum",
            scaling: serde_json::json!({ "minSize": 1, "maxSize": 3, "desiredSize": 9 }),
            refusal: Some("desired capacity 9 can't be greater than max size 3".to_string()),
        },
        Case {
            case: "a minimum alone, judged against the stored desired size",
            scaling: serde_json::json!({ "minSize": 3 }),
            refusal: Some(format!(
                "Minimum capacity 3 can't be greater than desired size {STORED_DESIRED}"
            )),
        },
        Case {
            case: "a maximum alone, judged against the stored desired size",
            scaling: serde_json::json!({ "maxSize": 1 }),
            refusal: Some(format!(
                "desired capacity {STORED_DESIRED} can't be greater than max size 1"
            )),
        },
        Case {
            case: "a desired size alone, above the stored maximum",
            scaling: serde_json::json!({ "desiredSize": 9 }),
            refusal: Some(format!(
                "desired capacity 9 can't be greater than max size {STORED_MAX}"
            )),
        },
        Case {
            case: "a desired size of zero, below the stored minimum",
            scaling: serde_json::json!({ "desiredSize": 0 }),
            refusal: Some(format!(
                "Minimum capacity {STORED_MIN} can't be greater than desired size 0"
            )),
        },
        Case {
            case: "three-digit capacities, the minimum above the desired size",
            scaling: serde_json::json!({ "minSize": 250, "maxSize": 400, "desiredSize": 100 }),
            refusal: Some(
                "Minimum capacity 250 can't be greater than desired size 100".to_string(),
            ),
        },
        Case {
            case: "three-digit capacities, the desired size above the maximum",
            scaling: serde_json::json!({ "minSize": 10, "maxSize": 120, "desiredSize": 300 }),
            refusal: Some(
                "desired capacity 300 can't be greater than max size 120".to_string(),
            ),
        },
        Case {
            case: "the desired size exactly on the lower bound",
            scaling: serde_json::json!({ "minSize": 2, "maxSize": 4, "desiredSize": 2 }),
            refusal: None,
        },
        Case {
            case: "the desired size exactly on the upper bound",
            scaling: serde_json::json!({ "minSize": 0, "maxSize": 2, "desiredSize": 2 }),
            refusal: None,
        },
        Case {
            case: "all three capacities equal",
            scaling: serde_json::json!({ "minSize": 3, "maxSize": 3, "desiredSize": 3 }),
            refusal: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// RULE: the refusal names the capacity the change asked for and the bound it falls
// outside of, and the two capacities of that relation — nothing else, and nothing
// from the emulator's own restatement of the constraint.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_scaling_change_is_refused_by_the_bound_its_desired_size_falls_outside() {
    let router = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let cluster = format!("scaling-capacity-{index}");
        active_nodegroup(&router, &cluster).await;

        let body = serde_json::json!({ "scalingConfig": row.scaling });
        let answer = update_config(&router, &cluster, "ng", &body).await;
        let case = row.case;
        match row.refusal {
            Some(expected) => {
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: expected 400 from UpdateNodegroupConfig, got {answer:?}"
                );
                assert_eq!(
                    error_code(&answer),
                    "InvalidParameterException",
                    "case {case:?}: expected InvalidParameterException, got {answer:?}"
                );
                assert_eq!(
                    answer.message(),
                    expected,
                    "case {case:?}: expected the refusal {expected:?}, got {:?}",
                    answer.message()
                );
                // A refused change writes nothing: the node group still reports the
                // capacities it was created with.
                let described = describe_nodegroup(&router, &cluster, "ng").await;
                assert_eq!(
                    described.body["nodegroup"]["scalingConfig"],
                    serde_json::json!({
                        "minSize": STORED_MIN,
                        "maxSize": STORED_MAX,
                        "desiredSize": STORED_DESIRED,
                    }),
                    "case {case:?}: a refused change must leave the capacities alone, got {:?}",
                    described.body["nodegroup"]["scalingConfig"]
                );
            }
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: capacities the node group can scale between must be \
                     accepted, got {answer:?} ({})",
                    answer.message()
                );
            }
        }
    }
}
