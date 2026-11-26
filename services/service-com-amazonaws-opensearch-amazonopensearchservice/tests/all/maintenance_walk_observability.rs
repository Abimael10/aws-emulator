// SPDX-License-Identifier: CC0-1.0

//! The WALK a node maintenance action goes through, as the client that started it sees it.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees. The harness and the per-operation helpers come from [`super::domain_lifecycle`]
//! and [`super::domain_maintenance`]; this suite adds no second copy of either.
//!
//! The rule under test: an accepted action is owed its run BY THE CLOCK, one
//! `crate::state::MAINTENANCE_STEP` per step, so the client that starts an action and polls
//! for its outcome reads the action under a status it is still under way in BEFORE it reads
//! the terminal status a finished action reports — and the listing then carries that same
//! action under that same terminal status. A step whose span were shorter than the gap
//! between two of that client's calls would leave the run unobservable: the first poll
//! after the call would already report a finished action.
//!
//! Asserted over a SET of inputs: every `MaintenanceType` the model publishes, each on a
//! data node of its own, and over the record's own reported instants rather than over the
//! wall clock.

use std::time::Duration;

use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    MAINTAINABLE_DATA_NODES, MAINTENANCE_STATUS_IN_PROGRESS, MAINTENANCE_STATUS_PENDING,
    MAINTENANCE_STATUS_SUCCESS, MAINTENANCE_STEP, NODE_TYPE_DATA,
};

use super::domain_lifecycle::{poll_until, string_member, Emulator};
use super::domain_maintenance::{census, create, every_role, list, start, status};

/// The `MaintenanceType` values the model publishes (`smithy_query(query="shape",
/// shape="MaintenanceType")`).
const MAINTENANCE_TYPES: [&str; 3] = ["REBOOT_NODE", "RESTART_SEARCH_PROCESS", "RESTART_DASHBOARD"];

/// The statuses of the modelled `MaintenanceStatus` enum an action is still UNDER WAY in —
/// what the client that started it keeps waiting on.
const UNDER_WAY: [&str; 2] = [MAINTENANCE_STATUS_PENDING, MAINTENANCE_STATUS_IN_PROGRESS];

/// How many steps the clock owes an accepted action: it begins, and then it finishes.
const STEPS: u32 = 2;

/// The gap between two calls of a client polling for an outcome a service takes minutes
/// over: the span each step of the walk is measured against, since a step shorter than this
/// is one such a client steps straight over.
const CLIENT_POLL_GAP: Duration = Duration::from_secs(1);

/// RULE: each step of an accepted action waits a span the client that started it can
/// observe — never shorter than the gap between two of that client's calls.
#[test]
fn each_step_waits_a_span_the_starting_client_can_observe() {
    assert!(
        MAINTENANCE_STEP >= CLIENT_POLL_GAP,
        "case the step span: a span shorter than the gap between two calls of a polling client \
         leaves the run unobservable, got {MAINTENANCE_STEP:?} against {CLIENT_POLL_GAP:?}"
    );
}

/// RULE: a client that starts an action reads it RUNNING before it reads it FINISHED, over
/// nothing but the status read, and the listing agrees on the terminal status.
#[tokio::test]
async fn an_accepted_action_is_read_running_before_it_is_read_finished() {
    let emulator = Emulator::new();
    let domain = "walked-maintenance-domain";
    create(&emulator, domain, every_role(MAINTAINABLE_DATA_NODES)).await;
    let data_nodes: Vec<String> = census(&emulator, domain)
        .await
        .into_iter()
        .filter(|(_, node_type)| node_type == NODE_TYPE_DATA)
        .map(|(id, _)| id)
        .collect();
    assert!(
        !data_nodes.is_empty(),
        "case DescribeDomainNodes({domain}): expected the census to publish a data node"
    );

    for (index, action) in MAINTENANCE_TYPES.iter().enumerate() {
        let case = format!("StartDomainMaintenance({action})");
        let node_id = data_nodes[index % data_nodes.len()].clone();
        let answer = start(&emulator, domain, action, Some(node_id.as_str())).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        let maintenance_id = string_member(&answer.body, "MaintenanceId", &case);

        // The read that follows the call that started the action reports it under a status
        // it is still under way in: the clock owes it its run, and the span of the first
        // step has not passed.
        let running = status(&emulator, domain, &maintenance_id).await;
        assert_eq!(
            running.status, 200,
            "case {case}: expected 200 from the read after the start, got {} with {:?}",
            running.status, running.body
        );
        let reported = string_member(&running.body, "Status", &case);
        assert!(
            UNDER_WAY.contains(&reported.as_str()),
            "case {case}: expected the action to be read under a status it is still under way \
             in, got {reported:?} in {:?}",
            running.body
        );

        // ... and nothing but that same read carries it to the terminal status.
        let case_poll = format!("GetDomainMaintenanceStatus({action})");
        poll_until(&case_poll, || async {
            let answer = status(&emulator, domain, &maintenance_id).await;
            assert_eq!(
                answer.status, 200,
                "case {case_poll}: expected 200 from every poll, got {} with {:?}",
                answer.status, answer.body
            );
            let reported = string_member(&answer.body, "Status", &case_poll);
            !UNDER_WAY.contains(&reported.as_str())
        })
        .await;

        let finished = status(&emulator, domain, &maintenance_id).await;
        assert_eq!(
            (
                finished.body["Status"].as_str(),
                finished.body["Action"].as_str(),
                finished.body["NodeId"].as_str(),
            ),
            (
                Some(MAINTENANCE_STATUS_SUCCESS),
                Some(*action),
                Some(node_id.as_str()),
            ),
            "case {case_poll}: expected the finished action to echo the request, got {:?}",
            finished.body
        );

        // The walk was owed one span per step, and each move is dated at the instant it
        // fell due — read off the record's OWN reported instants, so the case does not
        // depend on how fast the host runs.
        let created = finished.body["CreatedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {case_poll}: expected a CreatedAt, got {:?}", finished.body)
        });
        let updated = finished.body["UpdatedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {case_poll}: expected an UpdatedAt, got {:?}", finished.body)
        });
        let owed = f64::from(STEPS) * MAINTENANCE_STEP.as_secs_f64();
        assert!(
            (updated - created - owed).abs() < 1.0,
            "case {case_poll}: expected the terminal status to be dated {owed} s after the \
             action was created, got {} s in {:?}",
            updated - created,
            finished.body
        );

        // The listing carries the same action under the same terminal status.
        let listed = list(&emulator, domain, "").await;
        assert_eq!(
            listed.status, 200,
            "case ListDomainMaintenances({action}): expected 200, got {} with {:?}",
            listed.status, listed.body
        );
        let row = listed.body["DomainMaintenances"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row["MaintenanceId"].as_str() == Some(maintenance_id.as_str()))
                    .cloned()
            })
            .unwrap_or_else(|| {
                panic!(
                    "case ListDomainMaintenances({action}): expected the started action to be \
                     listed, got {:?}",
                    listed.body
                )
            });
        assert_eq!(
            row["Status"].as_str(),
            Some(MAINTENANCE_STATUS_SUCCESS),
            "case ListDomainMaintenances({action}): expected the listing to carry the terminal \
             status the read reports, got {row:?}"
        );
    }
}
