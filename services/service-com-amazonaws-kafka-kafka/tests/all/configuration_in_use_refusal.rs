// SPDX-License-Identifier: CC0-1.0

//! What `DeleteConfiguration` answers for a configuration a cluster was ATTACHED to after
//! it was already running, driven end to end over the same `MultiServiceRouter` the
//! emulator serves from, through the shared harness in `tests/all/kafka_harness.rs`.
//!
//! RULE under test: a configuration is in use while ANY cluster refers to it, however that
//! reference arose, so deleting one that is in use is refused with HTTP 400
//! `BadRequestException` carrying Amazon MSK's own two sentences for that condition — the
//! condition and the remediation — with the configuration left `ACTIVE` and readable.
//!
//! `configuration_deletion` drives that refusal for a configuration a cluster was CREATED
//! with, and `lifecycle_sequences_settle` for one cluster re-pointed from one configuration
//! to another. What this suite adds is the reference an `UpdateClusterConfiguration`
//! attaches over a TABLE: one cluster and two holding the same configuration, with a
//! neighbour configuration nothing refers to as the control, and the configuration a
//! re-pointed cluster moved TO becoming the held one in the same breath as the one it moved
//! FROM becomes free — so the refusal is shown to follow the reference each cluster
//! currently holds rather than the attachment path or the presence of clusters.

use serde_json::{json, Value};

// The service's own configuration state values, so the suite and the handlers cannot drift
// apart on what a configuration's `State` is called. The refusal MESSAGE is written out
// below instead: that text is what a client branches on, so it is pinned rather than read
// from the service.
use service_com_amazonaws_kafka_kafka::state::{
    CONFIGURATION_STATE_ACTIVE, CONFIGURATION_STATE_DELETING,
};

use crate::kafka_harness::{
    pct, poll_until_active, poll_until_operation_complete, ClusterRequest, ConfigurationRequest,
    Emulator, Reply, REGION,
};

/// The two sentences Amazon MSK refuses the delete of a configuration still attached to a
/// cluster with: the condition and what to do about it, entirely fixed text — no
/// configuration name, ARN, revision or cluster id is part of it.
const CONFIGURATION_IN_USE: &str = "Configuration is in use by one or more clusters. \
                                    Dissociate the configuration from the clusters.";

/// `DeleteConfiguration`, which the model binds to `DELETE /v1/configurations/{Arn}`.
async fn delete_configuration(emulator: &Emulator, arn: &str) -> Reply {
    emulator
        .call("DELETE", &format!("/v1/configurations/{}", pct(arn)), None)
        .await
}

/// `UpdateClusterConfiguration` (`PUT /v1/clusters/{ClusterArn}/configuration`): the call
/// that attaches a configuration revision to a cluster that is already running.
///
/// Waits the cluster operation it starts out, so the caller reads a cluster that is
/// actually running the revision rather than one still applying it.
async fn attach(emulator: &Emulator, cluster: &str, configuration: &str, case: &str) {
    let version = poll_until_active(emulator, cluster, case).await;
    let accepted = emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/configuration", pct(cluster)),
            Some(json!({
                "currentVersion": version,
                "configurationInfo": { "arn": configuration, "revision": 1 },
            })),
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "case {case}: expected UpdateClusterConfiguration to be accepted, got {} {}",
        accepted.status, accepted.body
    );
    let operation = accepted
        .at("/clusterOperationArn")
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case}: UpdateClusterConfiguration returned no clusterOperationArn")
        })
        .to_string();
    // Applying it is work the CLOCK owes the cluster, so this looks again until the
    // operation reports itself finished rather than waiting out a span.
    poll_until_operation_complete(emulator, &operation, case).await;
    let described = emulator.describe(cluster).await;
    assert_eq!(
        described
            .at("/clusterInfo/currentBrokerSoftwareInfo/configurationArn")
            .as_str(),
        Some(configuration),
        "case {case}: expected the cluster to run the attached configuration, got {}",
        described.body
    );
}

/// `DescribeConfiguration`'s `State` for `arn`, with the status it answered.
async fn state_of(emulator: &Emulator, arn: &str) -> (u16, Option<String>) {
    let reply = emulator.describe_configuration(arn).await;
    let state = reply
        .body
        .get("state")
        .and_then(Value::as_str)
        .map(str::to_string);
    (reply.status, state)
}

/// Assert the delete of `configuration` is refused for being in use, and that the refusal
/// changed nothing about it.
async fn assert_refused_as_in_use(emulator: &Emulator, configuration: &str, case: &str) {
    let refused = delete_configuration(emulator, configuration).await;
    assert_eq!(
        (
            refused.status,
            refused.error_code(),
            refused.error_message()
        ),
        (400, "BadRequestException", CONFIGURATION_IN_USE),
        "case {case}: expected MSK's in-use refusal, got {} {}",
        refused.status,
        refused.body
    );
    assert_eq!(
        state_of(emulator, configuration).await,
        (200, Some(CONFIGURATION_STATE_ACTIVE.to_string())),
        "case {case}: a refused delete must leave the configuration ACTIVE"
    );
}

#[tokio::test]
async fn a_configuration_an_update_attached_cannot_be_deleted() {
    // The rows differ along how many clusters hold the same attached configuration: the
    // refusal holds while ANY of them does.
    for clusters in [1_usize, 2] {
        let case = format!("{clusters} cluster(s) attached to the configuration by an update");
        let emulator = Emulator::new();
        let configuration = emulator
            .create_configuration_row_ok_in(
                REGION,
                &ConfigurationRequest::named(&format!("attached-{clusters}")),
            )
            .await;
        // A neighbour nothing refers to, in the same registry: the refusal must be about
        // the ONE configuration a cluster holds, not about there being clusters at all.
        let unused = emulator
            .create_configuration_row_ok_in(
                REGION,
                &ConfigurationRequest::named(&format!("unused-beside-{clusters}")),
            )
            .await;

        for index in 0..clusters {
            let cluster = emulator
                .create_ok(&ClusterRequest::named(&format!("attached-{clusters}-{index}")))
                .await;
            attach(&emulator, &cluster, &configuration, &case).await;
        }

        assert_refused_as_in_use(&emulator, &configuration, &case).await;
        // The neighbour is deletable throughout: it is the ASSOCIATION that refuses.
        let deletable = delete_configuration(&emulator, &unused).await;
        assert_eq!(
            (deletable.status, deletable.body["state"].as_str()),
            (200, Some(CONFIGURATION_STATE_DELETING)),
            "case {case}: a configuration no cluster refers to is deletable, got {} {}",
            deletable.status,
            deletable.body
        );
    }
}

#[tokio::test]
async fn re_pointing_a_cluster_moves_which_configuration_is_held() {
    let case = "a cluster moved from one configuration to another";
    let emulator = Emulator::new();
    let first = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("moved-from"))
        .await;
    let second = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("moved-to"))
        .await;
    let cluster = emulator
        .create_ok(&ClusterRequest::named("configuration-moved"))
        .await;

    attach(&emulator, &cluster, &first, case).await;
    assert_refused_as_in_use(&emulator, &first, case).await;

    // The cluster is re-pointed, so the reference moves with it: the configuration it
    // moved FROM is free, and the one it moved TO is now the held one.
    attach(&emulator, &cluster, &second, case).await;
    let accepted = delete_configuration(&emulator, &first).await;
    assert_eq!(
        (accepted.status, accepted.body["state"].as_str()),
        (200, Some(CONFIGURATION_STATE_DELETING)),
        "case {case}: the configuration nothing refers to must be deletable, got {} {}",
        accepted.status,
        accepted.body
    );
    assert_refused_as_in_use(&emulator, &second, case).await;
}
