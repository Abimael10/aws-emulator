// SPDX-License-Identifier: CC0-1.0

//! The resource-kind token a `ResourceNotFoundException` reports, read off the request's own
//! path.
//!
//! RULE: the kind this service names in a 404 — in the `resourceType` member and in the
//! sentence rendered from it — is the kind the addressed URI spells: the collection segment
//! of the path, in the singular, exactly as that segment is written (lower case, and
//! kebab-case where the segment is more than one word). It is never the camelCase of the
//! input member that carries the id.
//!
//! The model documents `resourceType` only as "The type of the resource that couldn't be
//! found" and enumerates no values (`smithy_query(query="shape",
//! shape="ResourceNotFoundException")`: `httpError` 404 with `message`, `resourceId` and
//! `resourceType` all `@required`), so the path is what spells the token, and a client that
//! switches on it — or that matches the sentence carrying it — must read the same text here
//! as against AWS.
//!
//! Where the sibling suites `resource_not_found_wording` and `not_found_message_template`
//! compare each refusal against a token WRITTEN OUT beside the row, this suite DERIVES the
//! expectation from the path it called: AWS names a collection in the plural and the resource
//! in the singular (`/storage-profiles/{storageProfileId}` addresses a `storage-profile`), so
//! dropping the segment's trailing `s` is the whole derivation. That makes the rule itself the
//! assertion — a token respelled in any style the path does not use fails here for every kind
//! at once, whichever handler fills it in.
//!
//! The rows are one per resource kind an implemented read path of this service refuses, so
//! the rule is asserted over the class and not over the one kind a divergence was seen on:
//! single-word kinds, the two-word `storage-profile`, the nested `environment` and `session`,
//! and the two composite association kinds the model mints no id for (named, as this service
//! names a composite, by the ids of their path). Every path is the modelled one
//! (`smithy_query(query="operation", operation="GetFarm,GetQueue,GetFleet,GetJob,GetWorker,\
//! GetBudget,GetLimit,GetStorageProfile,GetQueueEnvironment,GetSession,\
//! GetQueueFleetAssociation,GetQueueLimitAssociation")`), and the harness is the one the other
//! suites drive this service with, so every call goes through routing, deserialization,
//! constraint validation, the handler and serialization, exactly as the router does in
//! production.

use std::collections::BTreeSet;

use serde_json::json;

use crate::farm_queue_job::{absent_id, error_code, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::limit::create_limit;
use crate::resource_not_found_wording::not_found_message;

/// One row: a `GET` that names a resource nothing holds, split into the collection the path
/// addresses and the id segments that follow it.
struct Row {
    case: &'static str,
    /// The path up to and including the segment that names the collection.
    collection: String,
    /// The id segments the path ends in: one for a resource the model mints an id for,
    /// several for a composite resource it mints none for.
    ids: Vec<String>,
    /// The id the refusal must report — the id the path ends in, or, for a composite, the
    /// ids of its path joined the way this service joins them.
    resource_id: String,
}

impl Row {
    fn path(&self) -> String {
        format!("{}/{}", self.collection, self.ids.join("/"))
    }

    /// The kind this row's path spells: its collection segment, singular.
    fn kind(&self) -> String {
        let segment = self
            .collection
            .split('/')
            .next_back()
            .unwrap_or_else(|| panic!("case {:?}: a path has segments", self.case));
        segment
            .strip_suffix('s')
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: a collection segment is plural, got {segment:?}",
                    self.case
                )
            })
            .to_string()
    }
}

/// The ids of a composite resource, joined as this service names one: the first two
/// separated by a comma and the last by `and`.
fn composite_id(first: &str, second: &str, third: &str) -> String {
    format!("{first}, {second} and {third}")
}

/// The table: one row per resource kind an implemented read path of this service refuses.
///
/// Every id is well formed for its modelled `^<prefix>-[0-9a-f]{32}$` label pattern, so the
/// boundary validator passes the request and the handler is what decides the outcome; every
/// parent level of every path is held, so the kind reported is the one the path ends in.
async fn rows(emulator: &Emulator) -> Vec<Row> {
    let farm_id = emulator.farm("Path-kind farm").await;
    let queue_id = emulator.queue(&farm_id, "Path-kind queue").await;
    let fleet_id = create_fleet(emulator, &farm_id, "Path-kind fleet").await;
    let job_id = emulator.job(&farm_id, &queue_id, "path-kind-job", 50).await;
    let limit_id = create_limit(
        emulator,
        &farm_id,
        json!({
            "displayName": "Path-kind limit",
            "amountRequirementName": "amount.path",
            "maxCount": 5,
        }),
    )
    .await;

    let absent = |prefix: &str, digit: u8| -> String { absent_id(prefix, &format!("{digit:032x}")) };
    let absent_farm = absent("farm", 0x01);
    let absent_queue = absent("queue", 0x02);
    let absent_fleet = absent("fleet", 0x03);
    let absent_job = absent("job", 0x04);
    let absent_worker = absent("worker", 0x05);
    let absent_budget = absent("budget", 0x06);
    let absent_limit = absent("limit", 0x07);
    let absent_profile = absent("sp", 0x08);
    let absent_environment = absent("queueenv", 0x09);
    let absent_session = absent("session", 0x0a);

    vec![
        Row {
            case: "a farm",
            collection: "/2023-10-12/farms".to_string(),
            ids: vec![absent_farm.clone()],
            resource_id: absent_farm,
        },
        Row {
            case: "a queue under a farm that is held",
            collection: format!("/2023-10-12/farms/{farm_id}/queues"),
            ids: vec![absent_queue.clone()],
            resource_id: absent_queue,
        },
        Row {
            case: "a fleet under a farm that is held",
            collection: format!("/2023-10-12/farms/{farm_id}/fleets"),
            ids: vec![absent_fleet.clone()],
            resource_id: absent_fleet,
        },
        Row {
            case: "a job under a queue that is held",
            collection: format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs"),
            ids: vec![absent_job.clone()],
            resource_id: absent_job,
        },
        Row {
            case: "a worker in a fleet that is held",
            collection: format!("/2023-10-12/farms/{farm_id}/fleets/{fleet_id}/workers"),
            ids: vec![absent_worker.clone()],
            resource_id: absent_worker,
        },
        Row {
            case: "a budget under a farm that is held",
            collection: format!("/2023-10-12/farms/{farm_id}/budgets"),
            ids: vec![absent_budget.clone()],
            resource_id: absent_budget,
        },
        Row {
            case: "a limit under a farm that is held",
            collection: format!("/2023-10-12/farms/{farm_id}/limits"),
            ids: vec![absent_limit.clone()],
            resource_id: absent_limit,
        },
        Row {
            // The kind whose segment is two words: the token the path spells is kebab-case,
            // never the camelCase of the `storageProfileId` member that carries the id.
            case: "a storage profile under a farm that is held",
            collection: format!("/2023-10-12/farms/{farm_id}/storage-profiles"),
            ids: vec![absent_profile.clone()],
            resource_id: absent_profile,
        },
        Row {
            case: "a queue environment under a queue that is held",
            collection: format!(
                "/2023-10-12/farms/{farm_id}/queues/{queue_id}/environments"
            ),
            ids: vec![absent_environment.clone()],
            resource_id: absent_environment,
        },
        Row {
            case: "a session under a job that is held",
            collection: format!(
                "/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs/{job_id}/sessions"
            ),
            ids: vec![absent_session.clone()],
            resource_id: absent_session,
        },
        Row {
            // A composite kind: the model mints it no id, so the path ends in two ids and
            // the refusal names it by the three its path carries.
            case: "a queue-fleet association of a queue and a fleet that are both held",
            collection: format!("/2023-10-12/farms/{farm_id}/queue-fleet-associations"),
            ids: vec![queue_id.clone(), fleet_id.clone()],
            resource_id: composite_id(&farm_id, &queue_id, &fleet_id),
        },
        Row {
            case: "a queue-limit association of a queue and a limit that are both held",
            collection: format!("/2023-10-12/farms/{farm_id}/queue-limit-associations"),
            ids: vec![queue_id.clone(), limit_id.clone()],
            resource_id: composite_id(&farm_id, &queue_id, &limit_id),
        },
    ]
}

#[tokio::test]
async fn a_refusal_names_the_kind_its_path_spells() {
    let emulator = Emulator::new();

    // One entry per kind reported, so a row that reached the wrong level — or two rows that
    // collapsed onto one kind — is caught beside the per-row assertions.
    let mut kinds: BTreeSet<String> = BTreeSet::new();

    let rows = rows(&emulator).await;
    let row_count = rows.len();
    for row in &rows {
        let kind = row.kind();
        let path = row.path();
        let (status, body) = emulator.get(&path).await;
        assert_eq!(
            status, 404,
            "case {:?}: expected {path} to answer 404, got {status} with {body}",
            row.case
        );
        assert_eq!(
            (
                error_code(&body),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
                string_at(&body, "message"),
            ),
            (
                "ResourceNotFoundException".to_string(),
                kind.clone(),
                row.resource_id.clone(),
                not_found_message(&kind, &row.resource_id),
            ),
            "case {:?}: expected the 404 to name the kind {path} spells, got {body}",
            row.case
        );
        // The token carries no spelling the path does not use: no upper case (a camelCase
        // member name), no underscore and no space.
        let reported = string_at(&body, "resourceType");
        assert!(
            reported
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "case {:?}: expected the path's own spelling, got {reported:?}",
            row.case
        );
        kinds.insert(reported);
    }

    assert_eq!(
        kinds.len(),
        row_count,
        "expected one distinct kind per row, got {kinds:?}"
    );
}

/// A resource that WAS held and has been deleted is refused by the same kind as one that
/// never existed: the token follows the path, not the history of the id.
///
/// The rows differ along the kind and along how each resource is created, so the deleted-then-
/// read path is asserted for more than the one kind a divergence was seen on.
#[tokio::test]
async fn a_deleted_resource_is_named_by_the_kind_its_path_spells() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Deleted-kind farm").await;

    // (case, the collection the resource is created in, the create body, the member carrying
    // the minted id).
    let cases = [
        (
            "a storage profile",
            format!("/2023-10-12/farms/{farm_id}/storage-profiles"),
            json!({"displayName": "Gone profile", "osFamily": "LINUX"}),
            "storageProfileId",
        ),
        (
            "a limit",
            format!("/2023-10-12/farms/{farm_id}/limits"),
            json!({
                "displayName": "Gone limit",
                "amountRequirementName": "amount.gone",
                "maxCount": 2,
            }),
            "limitId",
        ),
        (
            "a queue",
            format!("/2023-10-12/farms/{farm_id}/queues"),
            json!({"displayName": "Gone queue"}),
            "queueId",
        ),
    ];

    for (case, collection, create_body, id_member) in &cases {
        let (status, body) = emulator.post(collection, create_body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to answer 200, got {status} with {body}"
        );
        let resource_id = string_at(&body, id_member);
        let row = Row {
            case: "a deleted resource",
            collection: collection.clone(),
            ids: vec![resource_id.clone()],
            resource_id: resource_id.clone(),
        };
        let kind = row.kind();
        let path = row.path();

        let (status, body) = emulator.call("DELETE", &path, None, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to answer 200, got {status} with {body}"
        );

        let (status, read) = emulator.get(&path).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the deleted resource to be gone, got {status} with {read}"
        );
        assert_eq!(
            (
                error_code(&read),
                string_at(&read, "resourceType"),
                string_at(&read, "resourceId"),
                string_at(&read, "message"),
            ),
            (
                "ResourceNotFoundException".to_string(),
                kind.clone(),
                resource_id.clone(),
                not_found_message(&kind, &resource_id),
            ),
            "case {case:?}: expected the deleted resource's 404 to name the kind {path} \
             spells, got {read}"
        );
    }
}
