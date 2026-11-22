// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the OpenSearch Service node maintenance surface:
//! `StartDomainMaintenance` and `GetDomainMaintenanceStatus`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a
//! client sees, not what a handler returns. The harness comes from
//! [`super::domain_lifecycle`]; this suite adds no second copy of it.
//!
//! The rules under test, each asserted over a SET of inputs rather than over one
//! call:
//!
//! * maintenance runs on ONE data node of the domain, named by `NodeId`: the id has
//!   to be one the node census publishes for a data node, and the data-node role has
//!   to be able to lose one of its nodes — so the eligible rows span the published
//!   node types (a data node, a master node, a warm node), the node counts on both
//!   sides of the smallest role a node can be taken out of, an id no census
//!   publishes, another domain's node and a request naming no node at all;
//! * an accepted action is a RECORD: the id the call hands out addresses it, it
//!   reports the action and node the request named, and the clock — not a reader —
//!   walks it from `PENDING` through `IN_PROGRESS` to the terminal status the service
//!   reports a finished action with, each move dated at the instant it fell due, over
//!   every `MaintenanceType` value the model publishes and over every state the clock
//!   can owe such a record;
//! * the listing reports those same records, each once and newest first, narrowed by
//!   `Action` and by `Status` — over every published value of each — and paged by
//!   every page size from one record per page up to more than the domain holds;
//! * an accepted action becomes TERMINAL on the reads a polling client makes and
//!   nothing else: a client that issues only `GetDomainMaintenanceStatus` reads it
//!   reach the terminal status this service reports a finished action with, and
//!   `ListDomainMaintenances` then carries that same action under that same status —
//!   over every published `MaintenanceType` and over the cluster shapes a maintainable
//!   domain comes in.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    Maintenance, MAINTAINABLE_DATA_NODES, MAINTENANCE_STATUS_IN_PROGRESS,
    MAINTENANCE_STATUS_PENDING, MAINTENANCE_STATUS_SUCCESS, NODE_TYPE_DATA, NODE_TYPE_MASTER,
    NODE_TYPE_ULTRAWARM,
};

use super::domain_lifecycle::{
    error_code, string_member, Answer, Emulator, PREFIX, RECORDED_NOT_FOUND_STATUS,
};

/// The `MaintenanceType` values the model publishes (`smithy_query(query="shape",
/// shape="MaintenanceType")`), every one of which names a process on a node.
pub(crate) const MAINTENANCE_TYPES: [&str; 3] =
    ["REBOOT_NODE", "RESTART_SEARCH_PROCESS", "RESTART_DASHBOARD"];

/// A `NodeId` no census publishes, inside `NodeId`'s `@length(min: 10, max: 40)`.
const ABSENT_NODE_ID: &str = "0123456789abcdef0123456789abcdef";

/// The `MaintenanceStatus` value the MODEL carries for a finished action
/// (`smithy_query(query="shape", shape="MaintenanceStatus")` lists `COMPLETED`,
/// `FAILED`, `IN_PROGRESS`, `PENDING` and `TIMED_OUT`), which the service's own
/// terminal status is not: a filter on it therefore selects no action at all.
const MODELLED_FINISHED_STATUS: &str = "COMPLETED";

/// `POST /2021-01-01/opensearch/domain/{DomainName}/domainMaintenance`.
pub(crate) async fn start(
    emulator: &Emulator,
    name: &str,
    action: &str,
    node_id: Option<&str>,
) -> Answer {
    let mut body = json!({ "Action": action });
    if let Some(node_id) = node_id {
        body["NodeId"] = json!(node_id);
    }
    emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/domain/{name}/domainMaintenance"),
            Some(body),
        )
        .await
}

/// `GET /2021-01-01/opensearch/domain/{DomainName}/domainMaintenance?maintenanceId=`.
pub(crate) async fn status(emulator: &Emulator, name: &str, maintenance_id: &str) -> Answer {
    emulator
        .call(
            "GET",
            &format!(
                "{PREFIX}/opensearch/domain/{name}/domainMaintenance?maintenanceId={maintenance_id}"
            ),
            None,
        )
        .await
}

/// `GET /2021-01-01/opensearch/domain/{DomainName}/domainMaintenances{query}`.
pub(crate) async fn list(emulator: &Emulator, name: &str, query: &str) -> Answer {
    emulator
        .call(
            "GET",
            &format!("{PREFIX}/opensearch/domain/{name}/domainMaintenances{query}"),
            None,
        )
        .await
}

/// `CreateDomain` with `cluster` as its `ClusterConfig`, asserting it was accepted,
/// and then waiting for the domain to report itself settled.
///
/// Every case below reads or acts on a NODE of the domain, and a domain with work in
/// flight publishes no node census — so each row waits for the CONDITION a client
/// waits for ([`super::domain_lifecycle::settled_domain`]) before the call under test,
/// rather than asserting on a domain that is still coming up.
pub(crate) async fn create(emulator: &Emulator, name: &str, cluster: Value) {
    let answer = emulator
        .create(json!({ "DomainName": name, "ClusterConfig": cluster }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case CreateDomain({name:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    super::domain_lifecycle::settled_domain(emulator, name).await;
}

/// The nodes of `name`, as `DescribeDomainNodes` publishes them: (NodeId, NodeType)
/// per node.
pub(crate) async fn census(emulator: &Emulator, name: &str) -> Vec<(String, String)> {
    let answer = emulator
        .call(
            "GET",
            &format!("{PREFIX}/opensearch/domain/{name}/nodes"),
            None,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case DescribeDomainNodes({name:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["DomainNodesStatusList"]
        .as_array()
        .unwrap_or_else(|| panic!("case DescribeDomainNodes({name:?}): expected a node list"))
        .iter()
        .map(|node| {
            let case = format!("DescribeDomainNodes({name:?})");
            (
                string_member(node, "NodeId", &case),
                string_member(node, "NodeType", &case),
            )
        })
        .collect()
}

/// The first node of `node_type` the census of `name` publishes.
async fn node_of(emulator: &Emulator, name: &str, node_type: &str) -> String {
    census(emulator, name)
        .await
        .into_iter()
        .find(|(_, published)| published == node_type)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("the census of {name} publishes a {node_type} node"))
}

/// A cluster configuration with `data_nodes` data nodes, dedicated masters and warm
/// nodes turned on — so the census publishes a node of every `NodeType` this service
/// reports.
pub(crate) fn every_role(data_nodes: i32) -> Value {
    json!({
        "InstanceType": "r6g.large.search",
        "InstanceCount": data_nodes,
        "DedicatedMasterEnabled": true,
        "DedicatedMasterType": "m5.large.search",
        "DedicatedMasterCount": 3,
        "WarmEnabled": true,
        "WarmType": "ultrawarm1.medium.search",
        "WarmCount": 3,
    })
}

#[tokio::test]
async fn maintenance_is_started_only_on_an_eligible_data_node() {
    let emulator = Emulator::new();
    // The published minimum a data node can be taken out of, and one domain per
    // count below it.
    let maintainable = MAINTAINABLE_DATA_NODES;
    create(&emulator, "big-domain", every_role(maintainable)).await;
    create(&emulator, "bigger-domain", every_role(maintainable + 4)).await;
    create(&emulator, "small-domain", every_role(1)).await;
    create(&emulator, "two-node-domain", every_role(maintainable - 1)).await;

    // ACCEPTED: a data node of a role that can lose one, at the bound and above it.
    for name in ["big-domain", "bigger-domain"] {
        let node_id = node_of(&emulator, name, NODE_TYPE_DATA).await;
        let answer = start(
            &emulator,
            name,
            "RESTART_SEARCH_PROCESS",
            Some(node_id.as_str()),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case StartDomainMaintenance({name:?}, data node): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        let maintenance_id = string_member(
            &answer.body,
            "MaintenanceId",
            &format!("StartDomainMaintenance({name:?})"),
        );
        assert!(
            (1..=100).contains(&maintenance_id.len()),
            "case StartDomainMaintenance({name:?}): a RequestId is 1..=100 characters, got \
             {maintenance_id:?}"
        );
    }

    // REFUSED: every other node a caller could name. One row per way a NodeId fails
    // to name a data node this service can maintain.
    let master_node = node_of(&emulator, "big-domain", NODE_TYPE_MASTER).await;
    let warm_node = node_of(&emulator, "big-domain", NODE_TYPE_ULTRAWARM).await;
    let small_data_node = node_of(&emulator, "small-domain", NODE_TYPE_DATA).await;
    let two_node_data_node = node_of(&emulator, "two-node-domain", NODE_TYPE_DATA).await;
    let other_domains_node = node_of(&emulator, "bigger-domain", NODE_TYPE_DATA).await;
    for (name, node_id, case) in [
        ("big-domain", master_node.as_str(), "a dedicated master node"),
        ("big-domain", warm_node.as_str(), "a warm node"),
        ("big-domain", ABSENT_NODE_ID, "an id no census publishes"),
        (
            "big-domain",
            other_domains_node.as_str(),
            "another domain's data node",
        ),
        (
            "small-domain",
            small_data_node.as_str(),
            "the only data node of its role",
        ),
        (
            "two-node-domain",
            two_node_data_node.as_str(),
            "a data node of a role one short of the bound",
        ),
    ] {
        let answer = start(&emulator, name, "RESTART_SEARCH_PROCESS", Some(node_id)).await;
        assert_eq!(
            answer.status, RECORDED_NOT_FOUND_STATUS,
            "case {case}: expected the missing resource's HTTP {RECORDED_NOT_FOUND_STATUS}, got \
             {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer),
            "ResourceNotFoundException",
            "case {case}: expected ResourceNotFoundException, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.body["message"].as_str(),
            Some("The requested node ID is invalid"),
            "case {case}: expected the service's wording for an ineligible node, got {:?}",
            answer.body
        );
    }

    // A request naming no node names nothing to act on, which is a malformed request
    // rather than a node that could not be found.
    let answer = start(&emulator, "big-domain", "RESTART_SEARCH_PROCESS", None).await;
    assert_eq!(
        answer.status, 400,
        "case no NodeId: expected 400, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(&answer),
        "ValidationException",
        "case no NodeId: expected ValidationException, got {:?}",
        answer.body
    );

    // A domain that was never created is the modelled not-found naming the domain.
    let answer = start(
        &emulator,
        "absent-domain",
        "REBOOT_NODE",
        Some(ABSENT_NODE_ID),
    )
    .await;
    assert_eq!(
        error_code(&answer),
        "ResourceNotFoundException",
        "case an unknown domain: expected ResourceNotFoundException, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some("Domain not found: absent-domain"),
        "case an unknown domain: expected the miss to name the domain, got {:?}",
        answer.body
    );
}

#[tokio::test]
async fn an_accepted_action_is_read_back_and_walked_by_the_clock() {
    let emulator = Emulator::new();
    create(&emulator, "maintained-domain", every_role(MAINTAINABLE_DATA_NODES)).await;
    let nodes: Vec<String> = census(&emulator, "maintained-domain")
        .await
        .into_iter()
        .filter(|(_, node_type)| node_type == NODE_TYPE_DATA)
        .map(|(id, _)| id)
        .collect();

    // One action per published `MaintenanceType`, each on a different data node of
    // the domain, so the record reports what its own request named.
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut started: Vec<(String, String, String)> = Vec::new();
    for (index, action) in MAINTENANCE_TYPES.iter().enumerate() {
        let node_id = nodes[index % nodes.len()].clone();
        let answer = start(
            &emulator,
            "maintained-domain",
            action,
            Some(node_id.as_str()),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case StartDomainMaintenance({action}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        let maintenance_id =
            string_member(&answer.body, "MaintenanceId", &format!("start {action}"));
        assert!(
            ids.insert(maintenance_id.clone()),
            "case StartDomainMaintenance({action}): every action has its own id, {maintenance_id} \
             repeated"
        );
        started.push((maintenance_id, (*action).to_string(), node_id));
    }

    // The clock, not the reader, walks the action: a client that polls reads it reach
    // the terminal status this service reports a finished action with, and every
    // status the walk went through is one this service reports.
    for (maintenance_id, action, node_id) in &started {
        let case = format!("GetDomainMaintenanceStatus({action})");
        let seen = polled_to_terminal(&emulator, "maintained-domain", maintenance_id, &case).await;
        assert_eq!(
            seen.last().map(String::as_str),
            Some(MAINTENANCE_STATUS_SUCCESS),
            "case {case}: expected the terminal status this service reports a finished action \
             with, the reads saw {seen:?}"
        );
        assert!(
            seen.iter()
                .all(|status| REPORTED_STATUSES.contains(&status.as_str())),
            "case {case}: every status read is one this service reports, the reads saw {seen:?}"
        );

        let answer = status(&emulator, "maintained-domain", maintenance_id).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            (
                answer.body["Status"].as_str(),
                answer.body["Action"].as_str(),
                answer.body["NodeId"].as_str(),
            ),
            (
                Some(MAINTENANCE_STATUS_SUCCESS),
                Some(action.as_str()),
                Some(node_id.as_str()),
            ),
            "case {case}: expected the action and node the request named, finished, got {:?}",
            answer.body
        );
        // The walk is dated: the instant the status last moved is the instant the step
        // fell due, so it is at or after the instant the action was created.
        let created = answer.body["CreatedAt"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: expected a CreatedAt, got {:?}", answer.body));
        let updated = answer.body["UpdatedAt"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: expected an UpdatedAt, got {:?}", answer.body));
        assert!(
            updated >= created,
            "case {case}: expected the status to be dated at or after the instant the action \
             was created, got {updated} after {created} in {:?}",
            answer.body
        );

        // A read does not move the record: the second answer is the first.
        let again = status(&emulator, "maintained-domain", maintenance_id).await;
        assert_eq!(
            again.body, answer.body,
            "case {case}: a read must not change the action: {:?} then {:?}",
            answer.body, again.body
        );
    }

    // Nor does any other call move it: an action the clock has finished has no step
    // owed to it any more, so every member a later read reports is the one the walk
    // left behind — the instant the status last moved included — however many other
    // calls this service has served in between.
    let (maintenance_id, action, _) = &started[0];
    let settled = status(&emulator, "maintained-domain", maintenance_id).await;
    let _ = list(&emulator, "maintained-domain", "").await;
    let _ = census(&emulator, "maintained-domain").await;
    let other_node = node_of(&emulator, "maintained-domain", NODE_TYPE_DATA).await;
    let _ = start(
        &emulator,
        "maintained-domain",
        MAINTENANCE_TYPES[0],
        Some(other_node.as_str()),
    )
    .await;
    let later = status(&emulator, "maintained-domain", maintenance_id).await;
    assert_eq!(
        later.body,
        settled.body,
        "case GetDomainMaintenanceStatus({action}, after further calls): the record of a \
         finished action does not move, got {:?} after {:?}",
        later.body,
        settled.body
    );
    assert_eq!(
        later.body["Status"].as_str(),
        Some(MAINTENANCE_STATUS_SUCCESS),
        "case GetDomainMaintenanceStatus({action}, after further calls): expected the terminal \
         status this service reports a finished action with, got {:?}",
        later.body
    );

    // An id this domain holds no action for, and an id another domain's action
    // carries, both name no resource.
    create(&emulator, "other-domain", every_role(MAINTAINABLE_DATA_NODES)).await;
    for (name, id, case) in [
        ("maintained-domain", ABSENT_NODE_ID, "an id nobody minted"),
        (
            "other-domain",
            maintenance_id.as_str(),
            "another domain's action",
        ),
    ] {
        let answer = status(&emulator, name, id).await;
        assert_eq!(
            answer.status, RECORDED_NOT_FOUND_STATUS,
            "case {case}: expected the missing resource's HTTP {RECORDED_NOT_FOUND_STATUS}, got \
             {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer),
            "ResourceNotFoundException",
            "case {case}: expected ResourceNotFoundException, got {:?}",
            answer.body
        );
    }
}

#[tokio::test]
async fn the_listing_reports_every_action_once_and_narrows_by_action_and_status() {
    let emulator = Emulator::new();
    create(&emulator, "listed-domain", every_role(MAINTAINABLE_DATA_NODES)).await;
    let nodes: Vec<String> = census(&emulator, "listed-domain")
        .await
        .into_iter()
        .filter(|(_, node_type)| node_type == NODE_TYPE_DATA)
        .map(|(id, _)| id)
        .collect();

    // Two actions per published `MaintenanceType`, so a filter on one action has
    // several to report and several to leave out.
    let mut started: Vec<(String, String)> = Vec::new();
    for action in MAINTENANCE_TYPES.iter().chain(MAINTENANCE_TYPES.iter()) {
        let node_id = nodes[started.len() % nodes.len()].clone();
        let answer = start(&emulator, "listed-domain", action, Some(node_id.as_str())).await;
        assert_eq!(
            answer.status, 200,
            "case StartDomainMaintenance({action}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        started.push((
            string_member(&answer.body, "MaintenanceId", action),
            (*action).to_string(),
        ));
    }

    // An unfiltered listing reports every action, each once; the newest first, so
    // the ids are the started ones reversed. The cases below are about the LISTING
    // rather than about the walk, so each action is first polled to the status the
    // clock leaves it in — a record read mid-step would make the comparisons below
    // depend on which step it was in.
    for (maintenance_id, action) in &started {
        polled_to_terminal(&emulator, "listed-domain", maintenance_id, action).await;
    }
    let answer = list(&emulator, "listed-domain", "").await;
    assert_eq!(
        answer.status, 200,
        "case ListDomainMaintenances(listed-domain): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let reported = answer.body["DomainMaintenances"]
        .as_array()
        .unwrap_or_else(|| panic!("case the whole listing: expected a list, got {:?}", answer.body))
        .clone();
    let ids: Vec<String> = reported
        .iter()
        .map(|details| string_member(details, "MaintenanceId", "the whole listing"))
        .collect();
    let expected: Vec<String> = started
        .iter()
        .rev()
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(
        ids, expected,
        "case the whole listing: expected every action once, newest first, got {ids:?}"
    );
    // Every row is the projection `GetDomainMaintenanceStatus` answers with, plus
    // the domain the action belongs to.
    for details in &reported {
        let id = string_member(details, "MaintenanceId", "the whole listing");
        let read = status(&emulator, "listed-domain", &id).await;
        for member in ["Action", "NodeId", "Status", "CreatedAt", "UpdatedAt"] {
            assert_eq!(
                details[member], read.body[member],
                "case {id}: the listing's {member} must be the one the status read reports \
                 ({:?}), got {:?}",
                read.body[member], details[member]
            );
        }
        assert_eq!(
            details["DomainName"].as_str(),
            Some("listed-domain"),
            "case {id}: every row names the domain the action ran on, got {details:?}"
        );
    }

    // `Action` and `Status` each narrow the listing to the actions carrying the
    // value named, and a value nothing carries selects nothing.
    for action in MAINTENANCE_TYPES {
        let answer = list(&emulator, "listed-domain", &format!("?action={action}")).await;
        let rows = answer.body["DomainMaintenances"]
            .as_array()
            .unwrap_or_else(|| panic!("case action={action}: expected a list, got {:?}", answer.body))
            .clone();
        assert_eq!(
            rows.len(),
            2,
            "case action={action}: expected the two actions started under it, got {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|details| details["Action"].as_str() == Some(action)),
            "case action={action}: every row carries the action asked for, got {rows:?}"
        );
    }
    // `Status` narrows the listing the same way. Every action here has been walked to
    // the terminal status the service reports a finished one with, so no status still
    // UNDER WAY selects any of them; the status a record is in while the clock still
    // owes it a step is asserted to select it in
    // [`the_clock_owes_each_step_and_a_read_reports_the_status_it_is_due`].
    for (query, expected, case) in [
        (
            format!("?status={MAINTENANCE_STATUS_PENDING}"),
            0,
            "no action still waiting to begin",
        ),
        (
            format!("?status={MAINTENANCE_STATUS_IN_PROGRESS}"),
            0,
            "no action still running",
        ),
        (
            format!("?status={MODELLED_FINISHED_STATUS}"),
            0,
            "no action carries the model's finished status",
        ),
        (
            format!("?action={}&status={MAINTENANCE_STATUS_PENDING}", MAINTENANCE_TYPES[0]),
            0,
            "both filters at once, on a status nothing carries",
        ),
    ] {
        let answer = list(&emulator, "listed-domain", &query).await;
        let rows = answer.body["DomainMaintenances"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: expected a list, got {:?}", answer.body))
            .len();
        assert_eq!(
            rows, expected,
            "case {case}: expected {expected} rows for {query}, got {rows}"
        );
    }
    // The status the records DO carry is not a value the enum publishes, so naming it
    // is a malformed request rather than a filter.
    let answer = list(
        &emulator,
        "listed-domain",
        &format!("?status={MAINTENANCE_STATUS_SUCCESS}"),
    )
    .await;
    assert_eq!(
        (answer.status, error_code(&answer).as_str()),
        (400, "ValidationException"),
        "case status={MAINTENANCE_STATUS_SUCCESS}: a status outside the modelled enum is \
         refused at the boundary, got {} with {:?}",
        answer.status,
        answer.body
    );

    // Every page size walks the listing exactly once, in the one order, plus the
    // uncapped default; a token this operation did not hand out is refused.
    for max_results in [None, Some(1), Some(2), Some(5), Some(100)] {
        let case = format!("maxResults {max_results:?}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut query = String::from("?");
            if let Some(size) = max_results {
                query.push_str(&format!("maxResults={size}&"));
            }
            if let Some(next) = token.as_deref() {
                query.push_str(&format!("nextToken={next}&"));
            }
            let answer = list(&emulator, "listed-domain", query.trim_end_matches(['?', '&'])).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected 200, got {} with {:?}",
                answer.status, answer.body
            );
            let page = answer.body["DomainMaintenances"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case}: expected a list, got {:?}", answer.body))
                .clone();
            if let Some(size) = max_results {
                assert!(
                    page.len() <= usize::try_from(size).unwrap_or(0),
                    "case {case}: a page holds no more than the maximum asked for, got {}",
                    page.len()
                );
            }
            for details in &page {
                seen.push(string_member(details, "MaintenanceId", &case));
            }
            pages += 1;
            assert!(
                pages <= started.len() + 1,
                "case {case}: paging terminates, got {pages} pages"
            );
            match answer.body["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, expected,
            "case {case}: the pages concatenated are every action, once each and in order"
        );
    }
    for (query, case) in [
        ("?nextToken=not-a-token", "a token nobody minted"),
        ("?maxResults=-1", "a negative maximum"),
        ("?maxResults=101", "a maximum past the modelled bound"),
    ] {
        let answer = list(&emulator, "listed-domain", query).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer),
            "ValidationException",
            "case {case}: expected ValidationException, got {:?}",
            answer.body
        );
    }

    // A status a record IS in selects it: an action the clock has not finished yet is
    // listed under the status it is waiting in, and the filter narrows to that one.
    // The record is planted, because the span a STARTED action waits in each step is
    // bounded by the next read a polling client makes — see
    // [`the_clock_owes_each_step_and_a_read_reports_the_status_it_is_due`], which is
    // where each step's own reporting is asserted.
    let waiting = "planted-waiting-action";
    let created_at = std::time::SystemTime::now();
    let ahead = std::time::Duration::from_secs(600);
    plant(
        &emulator,
        "listed-domain",
        Maintenance {
            maintenance_id: waiting.to_string(),
            action: MAINTENANCE_TYPES[0].to_string(),
            node_id: nodes[0].clone(),
            status: MAINTENANCE_STATUS_PENDING.to_string(),
            created_at,
            updated_at: created_at,
            starts_at: Some(created_at + ahead),
            completes_at: Some(created_at + ahead * 2),
        },
    );
    let answer = list(
        &emulator,
        "listed-domain",
        &format!("?status={MAINTENANCE_STATUS_PENDING}"),
    )
    .await;
    let ids: Vec<&str> = answer.body["DomainMaintenances"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case status={MAINTENANCE_STATUS_PENDING}, one action waiting: expected a list, \
                 got {:?}",
                answer.body
            )
        })
        .iter()
        .filter_map(|row| row["MaintenanceId"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec![waiting],
        "case status={MAINTENANCE_STATUS_PENDING}, one action waiting: expected the filter to \
         select it and leave the finished ones out, got {:?}",
        answer.body
    );

    // A domain with no action listed holds an empty list rather than a refusal, and
    // a name no domain is stored under is the generic envelope.
    create(&emulator, "quiet-domain", every_role(MAINTAINABLE_DATA_NODES)).await;
    let answer = list(&emulator, "quiet-domain", "").await;
    assert_eq!(
        (
            answer.status,
            answer.body["DomainMaintenances"].as_array().map(Vec::len)
        ),
        (200, Some(0)),
        "case a domain with no action: expected an empty listing, got {} with {:?}",
        answer.status,
        answer.body
    );
    let answer = list(&emulator, "absent-domain", "").await;
    assert_eq!(
        (answer.status, error_code(&answer).as_str()),
        (400, "BaseException"),
        "case an unknown domain: expected the generic envelope, got {} with {:?}",
        answer.status,
        answer.body
    );
}

/// Put `record` into `domain`'s maintenance history, reached exactly as a handler
/// reaches the store ([`Emulator::store`]).
///
/// The way this suite reaches a state no implemented operation parks a record in long
/// enough to read: the span a STARTED action spends waiting for each step the clock
/// owes it is bounded by the next read a polling client makes
/// (`crate::state::MAINTENANCE_STEP`), so a case that wants to read an action mid-walk
/// plants one whose due instants it chose rather than racing the host.
fn plant(emulator: &Emulator, domain: &str, record: Maintenance) {
    let store = emulator.store();
    let mut guard = store.lock();
    guard
        .domains
        .get_mut(domain)
        .unwrap_or_else(|| panic!("expected the store to hold domain {domain}"))
        .maintenances
        .push_back(record);
}

/// The statuses a maintenance action of this service is ever reported with: the
/// non-terminal ones the model publishes, and the terminal one the service answers a
/// finished action with.
pub(crate) const REPORTED_STATUSES: [&str; 3] = [
    MAINTENANCE_STATUS_PENDING,
    MAINTENANCE_STATUS_IN_PROGRESS,
    MAINTENANCE_STATUS_SUCCESS,
];

/// The statuses of [`REPORTED_STATUSES`] an action is still UNDER WAY in — what a
/// polling client keeps waiting on, and what every other status is terminal by
/// contrast with.
const NON_TERMINAL_STATUSES: [&str; 2] = [MAINTENANCE_STATUS_PENDING, MAINTENANCE_STATUS_IN_PROGRESS];

/// Read `maintenance_id` until it reports a TERMINAL status, issuing nothing but that
/// one read, and answer every distinct status the reads saw in the order they were
/// seen.
///
/// Waits for the CONDITION — a status the action can no longer move off — through the
/// suite's shared waiter ([`super::domain_lifecycle::poll_until`]), so the same case
/// passes on a fast host and on a loaded one and no second polling loop is written
/// here. Terminal means exactly what a polling client treats as terminal: a status
/// that is not one of the two the model publishes for an action still under way.
async fn polled_to_terminal(
    emulator: &Emulator,
    name: &str,
    maintenance_id: &str,
    case: &str,
) -> Vec<String> {
    let seen: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    super::domain_lifecycle::poll_until(case, || async {
        let answer = status(emulator, name, maintenance_id).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200 from every poll, got {} with {:?}",
            answer.status, answer.body
        );
        let reported = string_member(&answer.body, "Status", case);
        let mut seen = seen.borrow_mut();
        if seen.last() != Some(&reported) {
            seen.push(reported.clone());
        }
        !NON_TERMINAL_STATUSES.contains(&reported.as_str())
    })
    .await;
    seen.into_inner()
}

/// RULE: an accepted action is TERMINAL on the reads a polling client makes — the
/// first of them included — and nothing else.
///
/// What this adds over
/// [`an_accepted_action_is_read_back_and_walked_by_the_clock`], which walks the same
/// statuses off a domain of ONE shape: the client here issues nothing but the status read
/// between the start and the terminal status, the read and the listing are asserted to
/// agree on that terminal status, and the rule is asserted over the cluster shapes a
/// maintainable domain comes in. The two cases are therefore about one rule each — the
/// record a call leaves behind, and a polling client observing it — and should move
/// together if either changes.
#[tokio::test]
async fn an_accepted_action_becomes_terminal_on_a_polling_clients_reads_alone() {
    // One row per cluster shape a maintainable domain comes in — the role on its own at
    // the smallest count a node can be taken out of, the same role past that count, and a
    // cluster carrying every role this service publishes a node for — so the rule is
    // asserted over the shapes rather than over the one the recorded case used.
    let shapes: [(&str, &str, Value); 3] = [
        (
            "data nodes only",
            "polled-data-only",
            json!({
                "InstanceType": "t3.small.search",
                "InstanceCount": MAINTAINABLE_DATA_NODES,
            }),
        ),
        (
            "more data nodes than the smallest maintainable role",
            "polled-scaled-out",
            json!({
                "InstanceType": "t3.small.search",
                "InstanceCount": MAINTAINABLE_DATA_NODES + 2,
            }),
        ),
        (
            "every published role",
            "polled-every-role",
            every_role(MAINTAINABLE_DATA_NODES),
        ),
    ];

    for (shape, name, cluster) in shapes {
        let emulator = Emulator::new();
        // The domain is created the way the recorded client created it — a cluster, EBS
        // storage and an access policy — and the case waits for the CONDITION a client
        // waits for before it acts on a node of it.
        let answer = emulator
            .create(json!({
                "DomainName": name,
                "ClusterConfig": cluster,
                "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
                "AccessPolicies": "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
                    \"Allow\",\"Principal\":{\"AWS\":\"*\"},\"Action\":\"es:*\",\"Resource\":\
                    \"*\"}]}",
            }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case CreateDomain({shape}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        super::domain_lifecycle::settled_domain(&emulator, name).await;

        let data_nodes: Vec<String> = census(&emulator, name)
            .await
            .into_iter()
            .filter(|(_, node_type)| node_type == NODE_TYPE_DATA)
            .map(|(id, _)| id)
            .collect();
        assert!(
            !data_nodes.is_empty(),
            "case DescribeDomainNodes({shape}): expected the census to publish a data node"
        );

        // One action per published `MaintenanceType`, each started on a data node of the
        // census, as a client that read the census starts one.
        let mut started: Vec<(String, String, String)> = Vec::new();
        for (index, action) in MAINTENANCE_TYPES.iter().enumerate() {
            let node_id = data_nodes[index % data_nodes.len()].clone();
            let answer = start(&emulator, name, action, Some(node_id.as_str())).await;
            assert_eq!(
                answer.status, 200,
                "case StartDomainMaintenance({shape}, {action}): expected 200, got {} with {:?}",
                answer.status, answer.body
            );
            let maintenance_id = string_member(
                &answer.body,
                "MaintenanceId",
                &format!("StartDomainMaintenance({shape}, {action})"),
            );
            started.push((maintenance_id, (*action).to_string(), node_id));
        }

        for (maintenance_id, action, node_id) in &started {
            let case = format!("{shape} / {action}");
            // NOTHING but the status read: the action finishes on the reads a client
            // makes, with no further client-driven step.
            let seen = polled_to_terminal(&emulator, name, maintenance_id, &case).await;
            assert_eq!(
                seen.last().map(String::as_str),
                Some(MAINTENANCE_STATUS_SUCCESS),
                "case {case}: expected the terminal status this service reports a finished \
                 action with, the reads saw {seen:?}"
            );
            assert!(
                seen.iter()
                    .all(|status| REPORTED_STATUSES.contains(&status.as_str())),
                "case {case}: every status read is one this service reports, the reads saw \
                 {seen:?}"
            );

            // The terminal read still reports the action and the node the request named,
            // and the listing reports that same action under that same status.
            let read = status(&emulator, name, maintenance_id).await;
            assert_eq!(
                (
                    read.body["Status"].as_str(),
                    read.body["Action"].as_str(),
                    read.body["NodeId"].as_str(),
                ),
                (
                    Some(MAINTENANCE_STATUS_SUCCESS),
                    Some(action.as_str()),
                    Some(node_id.as_str()),
                ),
                "case {case}: expected the finished action to echo the request, got {:?}",
                read.body
            );

            let listed = list(&emulator, name, "").await;
            assert_eq!(
                listed.status, 200,
                "case ListDomainMaintenances({case}): expected 200, got {} with {:?}",
                listed.status, listed.body
            );
            let row = listed.body["DomainMaintenances"]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("case ListDomainMaintenances({case}): expected a list, got {:?}", listed.body)
                })
                .iter()
                .find(|row| row["MaintenanceId"].as_str() == Some(maintenance_id.as_str()))
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "case ListDomainMaintenances({case}): expected the started action to be \
                         listed, got {:?}",
                        listed.body
                    )
                });
            assert_eq!(
                row["Status"].as_str(),
                Some(MAINTENANCE_STATUS_SUCCESS),
                "case ListDomainMaintenances({case}): expected the listing to carry the same \
                 terminal status the read reports, got {row:?}"
            );
        }
    }
}

/// RULE: the CLOCK owes a maintenance action each step of its run, so the status a
/// read reports is the one the record's own due instants put it in at that moment,
/// each move dated at the instant it fell due rather than at the read that observed
/// it — and `ListDomainMaintenances` narrows its listing by exactly those statuses.
///
/// The rows are the states the clock can owe a record: both steps still to come, the
/// start fallen due with the completion still to come, and both fallen due. They are
/// planted through the store seam, the way a suite reaches a state no implemented
/// operation parks a record in ([`plant`]): the span a STARTED action spends waiting
/// for each step
/// is bounded by the next read a polling client makes
/// (`crate::state::MAINTENANCE_STEP`), because a maintenance action defers no state
/// change to those instants — so no case can read a started action mid-step without
/// depending on how fast the host runs.
/// [`an_accepted_action_is_read_back_and_walked_by_the_clock`] covers the started
/// action; this covers what each step of the walk reports.
#[tokio::test]
async fn the_clock_owes_each_step_and_a_read_reports_the_status_it_is_due() {
    let emulator = Emulator::new();
    let domain = "clock-walked-maintenance";
    create(&emulator, domain, every_role(MAINTAINABLE_DATA_NODES)).await;
    let node_id = node_of(&emulator, domain, NODE_TYPE_DATA).await;

    // Far enough either side of the planted creation instant that no row depends on
    // how long the case takes to run, and whole seconds, so the instants the answers
    // carry can be compared to the ones planted.
    let step = std::time::Duration::from_secs(600);
    let created_at = std::time::SystemTime::now() - step * 4;

    // One row per state the clock can owe the record: how many steps after the
    // creation instant each of the two falls due, the status a read must then report,
    // and how far after the creation instant that status must be dated. The creation
    // instant is four steps back, so a due instant beyond that is still to come.
    let rows: [(&str, u32, u32, &str, u32); 3] = [
        (
            "both steps still to come",
            5,
            6,
            MAINTENANCE_STATUS_PENDING,
            0,
        ),
        (
            "the start fell due, the completion still to come",
            1,
            5,
            MAINTENANCE_STATUS_IN_PROGRESS,
            1,
        ),
        ("both steps fell due", 1, 2, MAINTENANCE_STATUS_SUCCESS, 2),
    ];

    let mut planted: Vec<(String, &str, &str, u32)> = Vec::new();
    for (index, (case, starts_in, completes_in, expected_status, dated_after)) in
        rows.iter().enumerate()
    {
        let maintenance_id = format!("planted-action-{index}");
        plant(
            &emulator,
            domain,
            Maintenance {
                maintenance_id: maintenance_id.clone(),
                action: MAINTENANCE_TYPES[index % MAINTENANCE_TYPES.len()].to_string(),
                node_id: node_id.clone(),
                status: MAINTENANCE_STATUS_PENDING.to_string(),
                created_at,
                updated_at: created_at,
                starts_at: Some(created_at + step * *starts_in),
                completes_at: Some(created_at + step * *completes_in),
            },
        );
        planted.push((maintenance_id, case, expected_status, *dated_after));
    }

    for (maintenance_id, case, expected_status, dated_after) in &planted {
        let read = status(&emulator, domain, maintenance_id).await;
        assert_eq!(
            (read.status, read.body["Status"].as_str()),
            (200, Some(*expected_status)),
            "case {case}: expected the status the clock has made due, got {} with {:?}",
            read.status,
            read.body
        );
        // Each move is dated at the instant it fell DUE, not at the read that
        // materialised it, so the instant a caller reads does not move with the moment
        // it happened to look.
        let created = read.body["CreatedAt"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: expected a CreatedAt, got {:?}", read.body));
        let updated = read.body["UpdatedAt"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: expected an UpdatedAt, got {:?}", read.body));
        let expected_gap = f64::from(*dated_after) * step.as_secs_f64();
        assert!(
            (updated - created - expected_gap).abs() < 1.0,
            "case {case}: expected the status to be dated {expected_gap} s after the action was \
             created, got {} s in {:?}",
            updated - created,
            read.body
        );

        // A second read reports the same record: the reader moved nothing, and what
        // the clock had already made due was made due once.
        let again = status(&emulator, domain, maintenance_id).await;
        assert_eq!(
            again.body, read.body,
            "case {case}: a read must not move the record: {:?} then {:?}",
            read.body, again.body
        );

        // The listing narrows by that status: it selects this action, and the rows it
        // answers are the projection the status read reports.
        let listed = list(&emulator, domain, &format!("?status={expected_status}")).await;
        if *expected_status == MAINTENANCE_STATUS_SUCCESS {
            // The terminal status is not a value `MaintenanceStatus` publishes, so a
            // caller cannot name it: the request is refused at the boundary instead.
            assert_eq!(
                (listed.status, error_code(&listed).as_str()),
                (400, "ValidationException"),
                "case {case}: a status outside the modelled enum is refused, got {} with {:?}",
                listed.status,
                listed.body
            );
            continue;
        }
        let rows = listed.body["DomainMaintenances"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: expected a list, got {:?}", listed.body))
            .clone();
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|row| row["MaintenanceId"].as_str())
            .collect();
        assert_eq!(
            ids,
            vec![maintenance_id.as_str()],
            "case {case}: expected the status filter to select the action in that status, got \
             {rows:?}"
        );
        for member in ["Action", "NodeId", "Status", "CreatedAt", "UpdatedAt"] {
            assert_eq!(
                rows[0][member], read.body[member],
                "case {case}: the listing's {member} is the one the read reports ({:?}), got {:?}",
                read.body[member], rows[0][member]
            );
        }
    }
}
