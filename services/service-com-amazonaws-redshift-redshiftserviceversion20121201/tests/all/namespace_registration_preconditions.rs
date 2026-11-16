// SPDX-License-Identifier: CC0-1.0

//! The PRECONDITIONS of a namespace registration change, end to end over the
//! awsQuery wire: which cluster states a registration can be made from, and
//! what a release asks of the registration it releases.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! Two rules, each asserted over a table rather than over one request:
//!
//! * a provisioned cluster that is not serving cannot register its namespace at
//!   all, whichever state it is in and however it got there, and the refusal is
//!   the permanent one a client must not retry into — not the transient
//!   cluster-state fault;
//! * a release has NO registration precondition: a namespace that is not
//!   registered to the account the call names is already in the state the call
//!   asks for, so the call is accepted and reports the release as under way,
//!   however often it is repeated and whatever the registrations around it.
//!
//! The clock is stopped at the start of each case, so a cluster a case means to
//! read in a transient state stays in it however long the host takes over the
//! calls inside the case.
//!
//! [`crate::namespace_registration`] covers what a registration RECORDS and
//! [`crate::namespace_registration_serverless`] the resolution of the
//! serverless arm.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};
use crate::namespace_registration::{
    deregister, provisioned, register, serverless, settle, stored,
};

/// The consumer account the cases register their namespace to.
const CONSUMER: &str = "111122223333";

/// The sentence Amazon Redshift answers a registration of a cluster that cannot
/// serve one with.
///
/// Written out here independently of the handler: the text is entirely fixed —
/// it takes nothing from the request — so the whole sentence is the requirement.
const UNSUPPORTED: &str = "This cluster version doesn't support Register Namespace operation. \
                           Please upgrade to the latest cluster version.";

/// Create a cluster and leave it serving, with this emulator's clock stopped so
/// nothing moves under the case that follows.
async fn available_cluster(emulator: &Emulator, identifier: &str) {
    emulator.stop_clock();
    let created = emulator.create_minimal(identifier).await;
    assert_eq!(
        created.status, 200,
        "case seed {identifier}: expected the create to answer 200, got {} with body {}",
        created.status, created.body
    );
    settle(emulator);
    assert_eq!(
        cluster_status(emulator, identifier).await.as_deref(),
        Some(state::STATUS_AVAILABLE),
        "case seed {identifier}: expected the cluster to be serving before the case runs"
    );
}

/// The `ClusterStatus` a `DescribeClusters` reports for one cluster.
async fn cluster_status(emulator: &Emulator, cluster_identifier: &str) -> Option<String> {
    emulator
        .call(
            "DescribeClusters",
            &[("ClusterIdentifier", cluster_identifier)],
        )
        .await
        .text("ClusterStatus")
}

/// Assert one accepted release: 200 with the modelled `Deregistering` status.
fn assert_deregistering(case: &str, answer: &Answer) {
    assert_eq!(
        (answer.status, answer.text("Status")),
        (200, Some(state::NAMESPACE_DEREGISTERING.to_string())),
        "case {case}: expected 200 / Status={:?}, got {} / {:?} with body {}",
        state::NAMESPACE_DEREGISTERING,
        answer.status,
        answer.text("Status"),
        answer.body
    );
}

/// A cluster that is not serving cannot register its namespace: the call is
/// refused as one this cluster cannot serve, whichever state it is in.
#[tokio::test]
async fn a_cluster_that_is_not_serving_cannot_register_its_namespace() {
    // Each row brings ONE cluster into ONE state that is not `available`, and
    // the states span the ways a cluster gets there: still being provisioned, a
    // change being applied, a restart, and compute suspended.
    //
    // The state vocabulary is `Cluster$ClusterStatus`
    // (`smithy_query(query="doc", shape="Cluster", member="ClusterStatus")`).
    for (case, identifier, how, expected_status) in [
        (
            "still being provisioned",
            "nsreg-creating",
            "create",
            state::STATUS_CREATING,
        ),
        (
            "a registration already being applied",
            "nsreg-modifying",
            "register",
            state::STATUS_MODIFYING,
        ),
        (
            "restarting",
            "nsreg-rebooting",
            "reboot",
            state::STATUS_REBOOTING,
        ),
        (
            "compute suspended",
            "nsreg-paused",
            "pause",
            state::STATUS_PAUSED,
        ),
    ] {
        let emulator = Emulator::new();
        emulator.stop_clock();
        if how == "create" {
            let created = emulator.create_minimal(identifier).await;
            assert_eq!(
                created.status, 200,
                "case {case}: expected the create to answer 200, got {} with body {}",
                created.status, created.body
            );
        } else {
            available_cluster(&emulator, identifier).await;
        }
        match how {
            "register" => {
                let registered = register(&emulator, &provisioned(identifier), CONSUMER).await;
                assert_eq!(
                    registered.status, 200,
                    "case {case}: expected the first registration to answer 200, got {} with body {}",
                    registered.status, registered.body
                );
            }
            "reboot" => {
                let rebooted = emulator
                    .call("RebootCluster", &[("ClusterIdentifier", identifier)])
                    .await;
                assert_eq!(
                    rebooted.status, 200,
                    "case {case}: expected the reboot to answer 200, got {} with body {}",
                    rebooted.status, rebooted.body
                );
            }
            "pause" => {
                let paused = emulator
                    .call("PauseCluster", &[("ClusterIdentifier", identifier)])
                    .await;
                assert_eq!(
                    paused.status, 200,
                    "case {case}: expected the pause to answer 200, got {} with body {}",
                    paused.status, paused.body
                );
                // The pause is applied a span after the call, and the clock is
                // stopped, so it is crossed deliberately.
                settle(&emulator);
            }
            _ => {}
        }
        assert_eq!(
            cluster_status(&emulator, identifier).await.as_deref(),
            Some(expected_status),
            "case {case}: expected the cluster to report {expected_status:?} for the case, got {:?}",
            cluster_status(&emulator, identifier).await
        );

        let refused = register(&emulator, &provisioned(identifier), CONSUMER).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, Some("InvalidParameterValue".to_string())),
            "case {case}: expected 400 InvalidParameterValue, got {} / {:?} with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(UNSUPPORTED),
            "case {case}: expected the capability sentence, got {:?}",
            refused.text("Message")
        );

        // A cluster that is serving again takes the registration, so the
        // refusal was about the state and nothing else.
        if expected_status != state::STATUS_PAUSED {
            settle(&emulator);
            let accepted = register(&emulator, &provisioned(identifier), CONSUMER).await;
            assert_eq!(
                (accepted.status, accepted.text("Status")),
                (200, Some(state::NAMESPACE_REGISTERING.to_string())),
                "case {case}: expected the registration once the cluster serves again, got {} / {:?} with body {}",
                accepted.status,
                accepted.text("Status"),
                accepted.body
            );
        }
    }
}

/// A release asks nothing of the registration it releases: a namespace that is
/// not registered to the account the call names is already in the state the
/// call asks for.
#[tokio::test]
async fn a_release_of_a_namespace_that_is_not_registered_is_accepted() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "nsreg-release").await;
    let cluster = provisioned("nsreg-release");
    // No Redshift Serverless control plane is mounted here, so this pair is one
    // the emulator cannot resolve either way and is admitted — which is what
    // makes it a second ARM of the same rule rather than a second cluster.
    let namespace = serverless("nsreg-release-ns", "nsreg-release-wg");

    for (case, released) in [
        ("a cluster's namespace nothing registered", cluster.clone()),
        ("a serverless namespace nothing registered", namespace.clone()),
    ] {
        let provisioned_cluster = matches!(
            released,
            state::RegisteredNamespace::Provisioned { .. }
        );
        // Repeated, because an accepted call that is not idempotent breaks a
        // teardown that runs twice.
        for attempt in 1..=3 {
            let case = format!("{case}, attempt {attempt}");
            let answer = deregister(&emulator, &released, CONSUMER).await;
            assert_deregistering(&case, &answer);
            assert!(
                stored(&emulator, &released).is_empty(),
                "case {case}: expected a release of nothing to record nothing, got {:?}",
                stored(&emulator, &released)
            );
            // The call reached the service: a provisioned namespace's cluster
            // takes the change whether or not a registration was released, so a
            // client polling the cluster can tell the call apart from one that
            // never arrived. The clock is stopped, so the change is crossed
            // deliberately before the next attempt.
            if provisioned_cluster {
                assert_eq!(
                    cluster_status(&emulator, "nsreg-release").await.as_deref(),
                    Some(state::STATUS_MODIFYING),
                    "case {case}: expected the cluster to report the change, got {:?}",
                    cluster_status(&emulator, "nsreg-release").await
                );
            }
            settle(&emulator);
            if provisioned_cluster {
                assert_eq!(
                    cluster_status(&emulator, "nsreg-release").await.as_deref(),
                    Some(state::STATUS_AVAILABLE),
                    "case {case}: expected the cluster to serve again once the change fell due, got {:?}",
                    cluster_status(&emulator, "nsreg-release").await
                );
            }
        }
        // And the namespace still takes a registration afterwards: a release of
        // nothing left nothing behind.
        let registered = register(&emulator, &released, CONSUMER).await;
        assert_eq!(
            (registered.status, registered.text("Status")),
            (200, Some(state::NAMESPACE_REGISTERING.to_string())),
            "case {case}: expected the namespace to take a registration afterwards, got {} / {:?} with body {}",
            registered.status,
            registered.text("Status"),
            registered.body
        );
        settle(&emulator);
    }
}

/// The release releases the ONE pair it names and accepts every other: the
/// account that holds a registration loses it, an account that holds none is
/// accepted and loses nothing, and a repeated release of the same pair is
/// accepted too.
#[tokio::test]
async fn a_release_names_one_pair_and_leaves_every_other_registration_alone() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "nsreg-pairs").await;
    let cluster = provisioned("nsreg-pairs");
    let holder = "444455556666";
    let stranger = "777766665555";

    let registered = register(&emulator, &cluster, holder).await;
    assert_eq!(
        registered.status, 200,
        "case seed the registration: expected 200, got {} with body {}",
        registered.status, registered.body
    );
    settle(&emulator);

    // An account the namespace was never registered to is accepted and releases
    // nothing — the account that does hold it keeps it.
    let other = deregister(&emulator, &cluster, stranger).await;
    assert_deregistering("an account holding no registration", &other);
    assert_eq!(
        stored(&emulator, &cluster),
        vec![(state::NAMESPACE_REGISTERING.to_string(), holder.to_string())],
        "case an account holding no registration: expected the holder's registration untouched"
    );
    settle(&emulator);

    // The account that holds it loses it, and the record reports the release
    // until it falls due.
    let released = deregister(&emulator, &cluster, holder).await;
    assert_deregistering("the account holding the registration", &released);
    assert_eq!(
        stored(&emulator, &cluster),
        vec![(
            state::NAMESPACE_DEREGISTERING.to_string(),
            holder.to_string()
        )],
        "case the account holding the registration: expected the record to report the release"
    );
    settle(&emulator);
    assert!(
        stored(&emulator, &cluster).is_empty(),
        "case the account holding the registration: expected the record to be gone once the release fell due, got {:?}",
        stored(&emulator, &cluster)
    );

    // Releasing the same pair again is accepted: it is already released.
    let again = deregister(&emulator, &cluster, holder).await;
    assert_deregistering("the same pair released twice", &again);
    assert!(
        stored(&emulator, &cluster).is_empty(),
        "case the same pair released twice: expected it to record nothing, got {:?}",
        stored(&emulator, &cluster)
    );
}
