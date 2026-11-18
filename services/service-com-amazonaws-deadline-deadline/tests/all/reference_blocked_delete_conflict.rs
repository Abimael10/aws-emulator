// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the refusal class this service answers when a delete is
//! blocked by a queue that still references the resource.
//!
//! The rule under test, over every resource of this service a queue can hold: deleting a
//! resource one of its farm's queues still names is refused as a conflict over a resource
//! that is IN USE — HTTP 409 `ConflictException`, `reason` `RESOURCE_IN_USE`, with the
//! blocked resource as the `resourceId`/`resourceType` the model documents as "the
//! resource in use" and the fixed sentence of that error class, which names neither the
//! blocked resource nor the queues holding it. It is never a `ValidationException`: the
//! request itself is well formed, and what the caller has to change is the reference.
//!
//! The rows differ along the resource and along the KIND of reference that blocks the
//! delete — a list on the queue's own configuration (`allowedStorageProfileIds`), and the
//! two association resources a queue holds (queue-limit, queue-fleet) — and each row also
//! asserts the two halves of the contract around the refusal: the resource survives the
//! refused delete, and the identical delete succeeds once nothing references it.
//!
//! The harness (`Emulator`) and the per-resource create helpers are the ones the other
//! suites in this target drive the service with, so these tests go through routing,
//! deserialization, constraint validation, the handler and serialization, exactly as the
//! router does in production.

use serde_json::{json, Value};

use crate::farm_queue_job::{error_code, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::limit::create_limit;

/// Which resource a row deletes, and how a queue of its farm comes to reference it.
#[derive(Debug, Clone, Copy)]
enum Blocked {
    /// A queue's `allowedStorageProfileIds` names the profile.
    StorageProfile,
    /// A queue-limit association names the limit.
    Limit,
    /// A queue-fleet association names the fleet.
    Fleet,
}

/// One row: the resource, the prose noun the refusal's fixed sentence carries for it, and
/// the `resourceType` the conflict reports.
struct Case {
    blocked: Blocked,
    /// The noun of the fixed sentence, as this service spells the resource in prose.
    display_noun: &'static str,
    /// The `resourceType` the conflict reports, spelled the way the API path spells the
    /// resource (`/storage-profiles/`, `/limits/`, `/fleets/`).
    resource_type: &'static str,
}

/// Create the resource of `blocked` under `farm_id`, and return its id and the path
/// `DELETE` removes it at — the same path the read-back uses.
async fn create_blocked(
    emulator: &Emulator,
    farm_id: &str,
    blocked: Blocked,
) -> (String, String) {
    match blocked {
        Blocked::StorageProfile => {
            let (status, body) = emulator
                .post(
                    &format!("/2023-10-12/farms/{farm_id}/storage-profiles"),
                    // `osFamily` is required; its value does not enter the refusal.
                    json!({"displayName": "Blocked profile", "osFamily": "LINUX"}),
                )
                .await;
            assert_eq!(
                status, 200,
                "CreateStorageProfile: expected 200, got {status} with {body}"
            );
            let id = string_at(&body, "storageProfileId");
            let path = format!("/2023-10-12/farms/{farm_id}/storage-profiles/{id}");
            (id, path)
        }
        Blocked::Limit => {
            let id = create_limit(
                emulator,
                farm_id,
                json!({
                    "displayName": "Blocked limit",
                    "amountRequirementName": "amount.blocked",
                    "maxCount": 5,
                }),
            )
            .await;
            let path = format!("/2023-10-12/farms/{farm_id}/limits/{id}");
            (id, path)
        }
        Blocked::Fleet => {
            let id = create_fleet(emulator, farm_id, "Blocked fleet").await;
            let path = format!("/2023-10-12/farms/{farm_id}/fleets/{id}");
            (id, path)
        }
    }
}

/// Make a queue of `farm_id` reference `resource_id` the way `blocked` says, and return
/// that queue's id.
async fn reference_from_a_queue(
    emulator: &Emulator,
    farm_id: &str,
    blocked: Blocked,
    resource_id: &str,
) -> String {
    match blocked {
        Blocked::StorageProfile => {
            let (status, body) = emulator
                .post(
                    &format!("/2023-10-12/farms/{farm_id}/queues"),
                    json!({
                        "displayName": "Holder",
                        "allowedStorageProfileIds": [resource_id],
                    }),
                )
                .await;
            assert_eq!(
                status, 200,
                "CreateQueue allowing {resource_id}: expected 200, got {status} with {body}"
            );
            string_at(&body, "queueId")
        }
        Blocked::Limit => {
            let queue_id = emulator.queue(farm_id, "Holder").await;
            let (status, body) = emulator
                .call(
                    "PUT",
                    &format!("/2023-10-12/farms/{farm_id}/queue-limit-associations"),
                    Some(json!({"queueId": queue_id, "limitId": resource_id})),
                    None,
                )
                .await;
            assert_eq!(
                status, 200,
                "CreateQueueLimitAssociation: expected 200, got {status} with {body}"
            );
            queue_id
        }
        Blocked::Fleet => {
            let queue_id = emulator.queue(farm_id, "Holder").await;
            let (status, body) = emulator
                .call(
                    "PUT",
                    &format!("/2023-10-12/farms/{farm_id}/queue-fleet-associations"),
                    Some(json!({"queueId": queue_id, "fleetId": resource_id})),
                    None,
                )
                .await;
            assert_eq!(
                status, 200,
                "CreateQueueFleetAssociation: expected 200, got {status} with {body}"
            );
            queue_id
        }
    }
}

async fn delete_at(emulator: &Emulator, path: &str) -> (u16, Value) {
    emulator.call("DELETE", path, None, None).await
}

#[tokio::test]
async fn a_delete_a_queues_reference_blocks_is_a_resource_in_use_conflict() {
    let cases: Vec<Case> = vec![
        Case {
            blocked: Blocked::StorageProfile,
            display_noun: "Storage profile",
            resource_type: "storage-profile",
        },
        Case {
            blocked: Blocked::Limit,
            display_noun: "Limit",
            resource_type: "limit",
        },
        Case {
            blocked: Blocked::Fleet,
            display_noun: "Fleet",
            resource_type: "fleet",
        },
    ];

    for case in &cases {
        let name = case.blocked;
        let emulator = Emulator::new();
        let farm_id = emulator.farm("Reference-blocked delete farm").await;
        let (resource_id, resource_path) =
            create_blocked(&emulator, &farm_id, case.blocked).await;
        let queue_id =
            reference_from_a_queue(&emulator, &farm_id, case.blocked, &resource_id).await;

        let (status, body) = delete_at(&emulator, &resource_path).await;
        assert_eq!(
            status, 409,
            "case {name:?}: expected the blocked delete to answer 409, got {status} with \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {name:?}: expected a ConflictException, got {body}"
        );
        assert_eq!(
            string_at(&body, "message"),
            format!(
                "Request could not be completed: {} is associated with one or more Queues.",
                case.display_noun
            ),
            "case {name:?}: expected this error class's fixed sentence, got {body}"
        );
        assert_eq!(
            (
                string_at(&body, "reason"),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                "RESOURCE_IN_USE".to_string(),
                case.resource_type.to_string(),
                resource_id.clone(),
            ),
            "case {name:?}: expected the conflict to name the resource in use, got {body}"
        );
        // The sentence is fixed text for the class: neither the blocked resource's id nor
        // the holder's is interpolated into it.
        let message = string_at(&body, "message");
        assert!(
            !message.contains(&resource_id) && !message.contains(&queue_id),
            "case {name:?}: expected the message to interpolate no id, got {message:?}"
        );

        // The refused delete removed nothing.
        let (status, read) = emulator.get(&resource_path).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the refused resource to stay readable, got {status} \
             with {read}"
        );

        // The refusal follows the state, not the request: once the queue holding the
        // reference is gone, the identical delete succeeds and the resource reads back as
        // the modeled 404.
        let (status, body) = delete_at(
            &emulator,
            &format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: DeleteQueue expected 200, got {status} with {body}"
        );
        let (status, body) = delete_at(&emulator, &resource_path).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the unblocked delete to answer 200, got {status} with \
             {body}"
        );
        let (status, read) = emulator.get(&resource_path).await;
        assert_eq!(
            (status, error_code(&read)),
            (404, "ResourceNotFoundException".to_string()),
            "case {name:?}: expected the deleted resource to be gone, got {status} with \
             {read}"
        );
    }
}
