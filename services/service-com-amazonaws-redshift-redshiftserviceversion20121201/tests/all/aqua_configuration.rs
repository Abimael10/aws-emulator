// SPDX-License-Identifier: CC0-1.0

//! `ModifyAquaConfiguration`: the RETIRED AQUA modification, end to end over the
//! awsQuery wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! One rule is under test, and each row is one member of the class it
//! generalises over rather than one recorded request: a retired modification is
//! SERVED and keeps the preconditions of the resource it names — the cluster has
//! to be one this account holds (and the retired path's answer to one that is not
//! there is Amazon Redshift's internal-failure sentence,
//! `state::AQUA_CONFIGURATION_INTERNAL_FAILURE`, rather than the modelled
//! not-found fault) and it has to be serving (`InvalidClusterState`, 400) — and
//! loses only its effect, so past those it reports the configuration the cluster
//! already carries, whatever the request asked for, and writes nothing.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};
// The seed every cluster-scoped suite shares: create the cluster, then cross the
// clock-owed provisioning span by ADVANCING this emulator's own clock.
use crate::snapshot_copy_configuration::available_cluster;

/// The `AquaConfigurationStatus` values a request can carry, and the request
/// that carries none (the member is optional).
///
/// Source: `smithy_query(query="shape", shape="AquaConfigurationStatus")` — the
/// enum admits `auto`, `disabled` and `enabled`.
///
/// `pub(crate)` so a sibling suite about the SAME operation drives the same class
/// through this one table rather than keeping a second copy of it.
pub(crate) const REQUESTED_STATUSES: [Option<&str>; 4] =
    [Some("enabled"), Some("disabled"), Some("auto"), None];

/// The AQUA configuration every cluster carries, as the wire reports it: AQUA is
/// not in use, and which configuration to use is determined automatically.
const AUTOMATIC_CONFIGURATION: (&str, &str) = ("disabled", "auto");

/// `ModifyAquaConfiguration` for one cluster, asking for `status` when the row
/// carries one.
///
/// `pub(crate)` for the same reason as the table above: a sibling suite makes the
/// same request through this one builder.
pub(crate) async fn modify_aqua(emulator: &Emulator, cluster: &str, status: Option<&str>) -> Answer {
    let mut params: Vec<(&str, &str)> = vec![("ClusterIdentifier", cluster)];
    if let Some(status) = status {
        params.push(("AquaConfigurationStatus", status));
    }
    emulator.call("ModifyAquaConfiguration", &params).await
}

/// The pair of AQUA members an answer reports, so a modify and a read are
/// compared the same way.
fn reported_configuration(answer: &Answer) -> (Option<String>, Option<String>) {
    (
        answer.text("AquaStatus"),
        answer.text("AquaConfigurationStatus"),
    )
}

#[tokio::test]
async fn a_retired_aqua_modification_reports_the_clusters_own_configuration() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "aqua-cluster").await;

    let (expected_status, expected_configuration_status) = AUTOMATIC_CONFIGURATION;

    // The class: every configuration status a request can ask for, including the
    // request that asks for none. The answer cannot depend on it, because the
    // parameter is retired.
    for requested in REQUESTED_STATUSES {
        let case = format!("asking for {requested:?}");
        let answered = modify_aqua(&emulator, "aqua-cluster", requested).await;
        assert_eq!(
            answered.status, 200,
            "case {case}: expected the retired call to be served with 200, got {} / {:?} with \
             body {}",
            answered.status,
            answered.error_code(),
            answered.body
        );
        assert_eq!(
            reported_configuration(&answered),
            (
                Some(expected_status.to_string()),
                Some(expected_configuration_status.to_string())
            ),
            "case {case}: expected the cluster's automatic configuration, got body {}",
            answered.body
        );
    }

    // Nothing was written: the cluster still reports the same configuration, and
    // the retired calls did not move it out of `available` either.
    let read = emulator
        .call("DescribeClusters", &[("ClusterIdentifier", "aqua-cluster")])
        .await;
    assert_eq!(
        (reported_configuration(&read), read.text("ClusterStatus")),
        (
            (
                Some(expected_status.to_string()),
                Some(expected_configuration_status.to_string())
            ),
            Some(state::STATUS_AVAILABLE.to_string())
        ),
        "case the cluster after the retired calls: expected it untouched, got body {}",
        read.body
    );
}

#[tokio::test]
async fn a_retired_aqua_modification_refuses_a_cluster_this_account_does_not_hold() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "aqua-present").await;

    // The class: identifiers this account holds no cluster under, spanning the
    // documented identifier syntax (`state::CLUSTER_IDENTIFIER_RULE`) rather
    // than one spelling, crossed with the status asked for — which cannot change
    // the answer, because the cluster is resolved first.
    let longest: String = std::iter::once('c')
        .chain(std::iter::repeat_n(
            'x',
            state::CLUSTER_IDENTIFIER_RULE.max_len - 1,
        ))
        .collect();
    // The documented range is "from 1 to 63 alphanumeric characters or hyphens"
    // with the first character a letter (`state::CLUSTER_IDENTIFIER_RULE` carries
    // the upper bound), so the shortest identifier the rule admits is one letter.
    let shortest: String = "c".to_string();
    let missing: Vec<(&str, String)> = vec![
        (
            "a plain identifier nothing created",
            "no-aqua-here".to_string(),
        ),
        (
            "an identifier that differs from a present one by its last character",
            "aqua-presenta".to_string(),
        ),
        (
            "the shortest identifier the documented rule admits",
            shortest,
        ),
        ("the longest identifier the documented rule admits", longest),
    ];

    for (case, identifier) in missing {
        for requested in REQUESTED_STATUSES {
            let case = format!("{case} asking for {requested:?}");
            let refused = modify_aqua(&emulator, &identifier, requested).await;
            assert_eq!(
                (refused.status, refused.error_code().as_deref()),
                (500, Some("InternalFailure")),
                "case {case}: expected 500 InternalFailure, got {} / {:?} with body {}",
                refused.status,
                refused.error_code(),
                refused.body
            );
            // Fixed text end to end: the sentence names neither the identifier
            // the request asked about nor the operation.
            assert_eq!(
                refused.text("Message").as_deref(),
                Some(state::AQUA_CONFIGURATION_INTERNAL_FAILURE),
                "case {case}: expected Amazon Redshift's own sentence, got {:?}",
                refused.text("Message")
            );
            assert_eq!(
                reported_configuration(&refused),
                (None, None),
                "case {case}: expected no configuration in a refusal, got body {}",
                refused.body
            );
        }
    }

    // The cluster this account DOES hold is still answered with its
    // configuration: the refusals above are about the identifier, not about the
    // operation.
    let (expected_status, expected_configuration_status) = AUTOMATIC_CONFIGURATION;
    let answered = modify_aqua(&emulator, "aqua-present", Some("enabled")).await;
    assert_eq!(
        (answered.status, reported_configuration(&answered)),
        (
            200,
            (
                Some(expected_status.to_string()),
                Some(expected_configuration_status.to_string())
            )
        ),
        "case a cluster this account holds, after the refusals: expected 200 with the cluster's \
         automatic configuration, got body {}",
        answered.body
    );
}

#[tokio::test]
async fn a_retired_aqua_modification_needs_the_cluster_to_be_serving() {
    let emulator = Emulator::new();
    // The clock is stopped where it stands, so the cluster stays in the
    // TRANSIENT side of its provisioning span however long the host takes over
    // the calls below.
    emulator.stop_clock();
    let created = emulator.create_minimal("aqua-provisioning").await;
    assert_eq!(
        created.status, 200,
        "case seed cluster aqua-provisioning: expected 200, got {} with body {}",
        created.status, created.body
    );

    for requested in REQUESTED_STATUSES {
        let case = format!("a cluster still being provisioned, asking for {requested:?}");
        let refused = modify_aqua(&emulator, "aqua-provisioning", requested).await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (400, Some("InvalidClusterState")),
            "case {case}: expected 400 InvalidClusterState, got {} / {:?} with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.text("Message"),
            Some(state::invalid_cluster_state_message(
                "aqua-provisioning",
                state::STATUS_CREATING
            )),
            "case {case}: expected the cluster and the state it is in named in the sentence, got \
             {:?}",
            refused.text("Message")
        );
    }

    // Once the provisioning span has been crossed the same call is served: the
    // precondition is the cluster's state, not the call's name.
    emulator.advance_clock(state::PROVISIONING_SPAN);
    emulator
        .poll_until(
            "seed cluster aqua-provisioning",
            "DescribeClusters",
            &[("ClusterIdentifier", "aqua-provisioning")],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;

    let (expected_status, expected_configuration_status) = AUTOMATIC_CONFIGURATION;
    for requested in REQUESTED_STATUSES {
        let case = format!("the cluster now serving, asking for {requested:?}");
        let answered = modify_aqua(&emulator, "aqua-provisioning", requested).await;
        assert_eq!(
            (answered.status, reported_configuration(&answered)),
            (
                200,
                (
                    Some(expected_status.to_string()),
                    Some(expected_configuration_status.to_string())
                )
            ),
            "case {case}: expected 200 with the cluster's automatic configuration, got body {}",
            answered.body
        );
    }
}
