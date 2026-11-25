// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE `RebootCluster` refuses a cluster that is not serving with, end
//! to end over the awsQuery wire.
//!
//! The RULE under test: a restart is admitted only on a cluster that is
//! `available`, and the `InvalidClusterState` refusal carries this OPERATION's
//! own sentence — the fixed text `Cluster ` and
//! ` is not in available state to reboot.` around the `ClusterIdentifier` the
//! request named. It names the state a restart is legal FROM and not the state
//! the cluster happens to be in, so one sentence covers every state the work
//! cannot start from and a client matching the text recognises the state gate
//! whichever of them the cluster is in.
//!
//! The rows differ along what the rule generalises over: which state the cluster
//! is in (still being provisioned, suspended, already restarting), and the
//! identifier's spelling. The other half of the rule is asserted too: the same
//! cluster, once it is serving, takes the restart.
//!
//! The harness is shared with [`crate::cluster_lifecycle`], which owns it.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};

/// The sentence the state gate refuses with, spelled out here rather than read
/// off the crate under test, so a row cannot pass by restating whatever the
/// handler happened to build.
fn state_gate_refusal(cluster_identifier: &str) -> String {
    format!("Cluster {cluster_identifier} is not in available state to reboot.")
}

/// `RebootCluster` for one cluster identifier.
async fn reboot(emulator: &Emulator, identifier: &str) -> Answer {
    emulator
        .call("RebootCluster", &[("ClusterIdentifier", identifier)])
        .await
}

/// Create one cluster and bring it to the state a restart is admitted in, by
/// ADVANCING this emulator's clock across the provisioning span rather than
/// waiting for it.
async fn available_cluster(emulator: &Emulator, identifier: &str) {
    let created = emulator.create_minimal(identifier).await;
    assert_eq!(
        created.status, 200,
        "case seed {identifier}: expected 200, got {} with body {}",
        created.status, created.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);
}

#[tokio::test]
async fn the_state_gate_refusal_is_this_operations_own_sentence() {
    let emulator = Emulator::new();

    // A cluster whose compute is suspended: paused, and the pause allowed to
    // fall due.
    let paused = "reboot-wording-first";
    available_cluster(&emulator, paused).await;
    let suspended = emulator
        .call("PauseCluster", &[("ClusterIdentifier", paused)])
        .await;
    assert_eq!(
        suspended.status, 200,
        "case seed the paused cluster: expected 200, got {} with body {}",
        suspended.status, suspended.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);

    // A cluster the service is already restarting, and one still being
    // provisioned. The clock is STOPPED from here, so neither transient state
    // can fall due while the table runs — whatever the host is doing.
    let restarting = "reboot-wording-second";
    available_cluster(&emulator, restarting).await;
    emulator.stop_clock();
    let accepted = reboot(&emulator, restarting).await;
    assert_eq!(
        accepted.status, 200,
        "case seed the restarting cluster: expected its first restart to be accepted, got {} \
         with body {}",
        accepted.status, accepted.body
    );
    let provisioning = "reboot-wording-third";
    let created = emulator.create_minimal(provisioning).await;
    assert_eq!(
        created.status, 200,
        "case seed the provisioning cluster: expected 200, got {} with body {}",
        created.status, created.body
    );

    // (case, the cluster the row restarts)
    for (case, identifier) in [
        ("a cluster whose compute is suspended", paused),
        ("a cluster the service is already restarting", restarting),
        ("a cluster still being provisioned", provisioning),
    ] {
        let refused = reboot(&emulator, identifier).await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (400, Some("InvalidClusterState")),
            "case {case}: expected 400 InvalidClusterState, got {} / {:?} with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        let expected = state_gate_refusal(identifier);
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(expected.as_str()),
            "case {case}: expected the message {expected:?}, got {:?} with body {}",
            refused.text("Message"),
            refused.body
        );
        // The sentence does NOT report the state the cluster is in: one text
        // covers every state the restart cannot start from, so a client matching
        // it recognises the gate whichever state that is. (The identifiers above
        // name no state, so a state named in the sentence could only be the
        // handler's own.)
        let read = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        let reported = read.text("ClusterStatus").unwrap_or_default();
        assert!(
            !refused
                .text("Message")
                .unwrap_or_default()
                .contains(&reported),
            "case {case}: expected the refusal not to name the current state {reported:?}, got \
             {:?}",
            refused.text("Message")
        );
    }

    // The other half of the rule: each of those clusters takes the restart once
    // it is serving again — so the refusals above were about the state and not
    // about the operation.
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let resumed = emulator
        .call("ResumeCluster", &[("ClusterIdentifier", paused)])
        .await;
    assert_eq!(
        resumed.status, 200,
        "case resume the paused cluster: expected 200, got {} with body {}",
        resumed.status, resumed.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);
    for identifier in [paused, restarting, provisioning] {
        let served = reboot(&emulator, identifier).await;
        assert_eq!(
            served.status, 200,
            "case the serving cluster {identifier}: expected the restart to be accepted, got {} \
             with body {}",
            served.status, served.body
        );
    }
}
