// SPDX-License-Identifier: CC0-1.0

//! The awsQuery ENVELOPE a served `ModifyAquaConfiguration` answers with, end to
//! end over the wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! The RULE under test: a call this service SERVES answers HTTP 200 whose body is
//! the query protocol's one success envelope for the operation —
//! `<ModifyAquaConfigurationResponse>` holding
//! `<ModifyAquaConfigurationResult>`, holding the operation's modelled output
//! (`ModifyAquaOutputMessage$AquaConfiguration`, whose members are
//! `AquaStatus` and `AquaConfigurationStatus`) beside exactly one
//! `ResponseMetadata/RequestId` — and never an internal-failure status or an
//! error envelope, for any cluster this account holds and whatever the retired
//! request asked for. [`crate::aqua_configuration`] asserts WHICH configuration
//! that envelope reports; this suite asserts that the answer is a served
//! envelope of the modelled shape at all, which is what a client parses before
//! it can read a member out of it.
//!
//! The rows span what the rule generalises over: every configuration status a
//! request can carry (including the request that carries none), clusters that
//! reached the serving state by different routes and that asked for different
//! retired statuses when they were created, and the same cluster called more
//! than once.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

// The retired-status table and the request builder both suites about this
// operation share, so the two cannot drift on what the enum admits.
use crate::aqua_configuration::{modify_aqua, REQUESTED_STATUSES};
use crate::cluster_lifecycle::{element_blocks, Answer, Emulator};

/// The values `AquaConfiguration$AquaStatus` may report.
///
/// Source: `smithy_query(query="shape", shape="AquaStatus")` — the enum admits
/// `enabled`, `disabled` and `applying`.
const AQUA_STATUS_VALUES: [&str; 3] = ["enabled", "disabled", "applying"];

/// The values `AquaConfiguration$AquaConfigurationStatus` may report.
///
/// Source: `smithy_query(query="shape", shape="AquaConfigurationStatus")`.
const AQUA_CONFIGURATION_STATUS_VALUES: [&str; 3] = ["enabled", "disabled", "auto"];

/// One row of the cluster table: the case name, the cluster's identifier and the
/// retired `AquaConfigurationStatus` its CREATE asked for (`None` when the create
/// asked for none).
type ClusterRow = (&'static str, &'static str, Option<&'static str>);

/// The clusters every case of this suite is driven against: the create asked for
/// each value of the retired status, and for none.
const CLUSTERS: [ClusterRow; 4] = [
    ("a cluster created asking for no AQUA status", "aqua-env-plain", None),
    (
        "a cluster created asking for AQUA enabled",
        "aqua-env-enabled",
        Some("enabled"),
    ),
    (
        "a cluster created asking for AQUA disabled",
        "aqua-env-disabled",
        Some("disabled"),
    ),
    (
        "a cluster created asking for AQUA auto",
        "aqua-env-auto",
        Some("auto"),
    ),
];

/// Create every cluster of [`CLUSTERS`], cross the clock-owed provisioning span
/// by ADVANCING this emulator's own clock, and wait for each to REPORT the
/// serving state through its own read.
async fn serving_clusters(emulator: &Emulator) {
    for (case, identifier, created_with) in CLUSTERS {
        let mut params: Vec<(&str, &str)> = vec![
            ("ClusterIdentifier", identifier),
            ("NodeType", "ra3.xlplus"),
            ("MasterUsername", "admin"),
        ];
        if let Some(status) = created_with {
            params.push(("AquaConfigurationStatus", status));
        }
        let created = emulator.call("CreateCluster", &params).await;
        assert_eq!(
            created.status, 200,
            "case {case}: the cluster must be created, got {} with body {}",
            created.status, created.body
        );
    }
    emulator.advance_clock(state::PROVISIONING_SPAN);
    for (case, identifier, _) in CLUSTERS {
        emulator
            .poll_until(
                case,
                "DescribeClusters",
                &[("ClusterIdentifier", identifier)],
                |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
            )
            .await;
    }
}

/// The inner XML of the ONE `<name>` element of `xml`, asserting there is exactly
/// one.
fn only_block(case: &str, xml: &str, name: &str) -> String {
    let mut blocks = element_blocks(xml, name);
    assert_eq!(
        blocks.len(),
        1,
        "case {case}: expected exactly one <{name}> element, got {} in {xml:?}",
        blocks.len()
    );
    blocks.remove(0)
}

/// The inner XML reached by descending `path`, one nested element at a time, so a
/// member served at the wrong DEPTH fails rather than being found anywhere in the
/// body.
fn nested_block(case: &str, xml: &str, path: &[&str]) -> String {
    let mut inner = xml.to_string();
    for name in path {
        inner = only_block(&format!("{case} at <{name}>"), &inner, name);
    }
    inner
}

// The one text of the `<name>` member of a structure's inner XML, read through the
// crate's own single-element reader rather than a second copy of it.
use crate::reserved_nodes::one as only_text;

/// The `AquaConfiguration` an answer reports, asserting the envelope nesting on
/// the way down: the members are read out of the element reached through `path`,
/// not from anywhere in the body.
fn configuration_at(case: &str, answer: &Answer, path: &[&str]) -> (String, String) {
    let configuration = nested_block(case, &answer.body, path);
    let aqua_status = only_text(case, &configuration, "AquaStatus");
    let configuration_status = only_text(case, &configuration, "AquaConfigurationStatus");
    assert!(
        AQUA_STATUS_VALUES.contains(&aqua_status.as_str()),
        "case {case}: expected an AquaStatus the model admits ({AQUA_STATUS_VALUES:?}), got \
         {aqua_status:?}"
    );
    assert!(
        AQUA_CONFIGURATION_STATUS_VALUES.contains(&configuration_status.as_str()),
        "case {case}: expected an AquaConfigurationStatus the model admits \
         ({AQUA_CONFIGURATION_STATUS_VALUES:?}), got {configuration_status:?}"
    );
    (aqua_status, configuration_status)
}

/// The path the operation's output travels in a query-protocol success envelope.
const MODIFY_PATH: [&str; 3] = [
    "ModifyAquaConfigurationResponse",
    "ModifyAquaConfigurationResult",
    "AquaConfiguration",
];

/// The path a cluster's own configuration travels in a `DescribeClusters` answer.
const DESCRIBE_PATH: [&str; 5] = [
    "DescribeClustersResponse",
    "DescribeClustersResult",
    "Clusters",
    "Cluster",
    "AquaConfiguration",
];

#[tokio::test]
async fn a_served_retired_aqua_modification_answers_the_modelled_success_envelope() {
    let emulator = Emulator::new();
    serving_clusters(&emulator).await;

    for (cluster_case, identifier, _) in CLUSTERS {
        for requested in REQUESTED_STATUSES {
            let case = format!("{cluster_case}, asking for {requested:?}");
            let answered = modify_aqua(&emulator, identifier, requested).await;
            assert_eq!(
                answered.status, 200,
                "case {case}: expected the retired call to be served with 200, got {} / {:?} with \
                 body {}",
                answered.status,
                answered.error_code(),
                answered.body
            );
            assert!(
                !answered.body.contains("<ErrorResponse"),
                "case {case}: a served call must carry no error envelope, got body {}",
                answered.body
            );
            // The output shape, read only at the depth the protocol puts it.
            configuration_at(&case, &answered, &MODIFY_PATH);
            // One request id, inside the response envelope's own metadata.
            let metadata = nested_block(
                &case,
                &answered.body,
                &["ModifyAquaConfigurationResponse", "ResponseMetadata"],
            );
            let request_id = only_text(&case, &metadata, "RequestId");
            assert!(
                !request_id.trim().is_empty(),
                "case {case}: expected a non-empty ResponseMetadata/RequestId, got \
                 {request_id:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_served_envelope_and_the_cluster_read_report_one_configuration() {
    let emulator = Emulator::new();
    serving_clusters(&emulator).await;

    for (cluster_case, identifier, _) in CLUSTERS {
        let read = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_eq!(
            read.status, 200,
            "case {cluster_case}: the cluster read must be served, got {} with body {}",
            read.status, read.body
        );
        let expected = configuration_at(
            &format!("{cluster_case} read before the modifications"),
            &read,
            &DESCRIBE_PATH,
        );

        // Asking for each status in turn, twice over the whole table, reports the
        // same configuration every time: the retired parameter changes nothing,
        // so neither the first answer nor a later one can differ.
        for round in ["the first round", "a second round"] {
            for requested in REQUESTED_STATUSES {
                let case = format!("{cluster_case}, {round}, asking for {requested:?}");
                let answered = modify_aqua(&emulator, identifier, requested).await;
                assert_eq!(
                    answered.status, 200,
                    "case {case}: expected 200, got {} / {:?} with body {}",
                    answered.status,
                    answered.error_code(),
                    answered.body
                );
                assert_eq!(
                    configuration_at(&case, &answered, &MODIFY_PATH),
                    expected,
                    "case {case}: expected the cluster's own configuration {expected:?}, got body \
                     {}",
                    answered.body
                );
            }
        }

        // And the cluster still reports it after the retired calls: nothing was
        // written, so the modify and the read cannot disagree.
        let after = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_eq!(
            configuration_at(
                &format!("{cluster_case} read after the modifications"),
                &after,
                &DESCRIBE_PATH
            ),
            expected,
            "case {cluster_case}: expected the cluster untouched, got body {}",
            after.body
        );
    }
}
