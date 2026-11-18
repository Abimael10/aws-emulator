// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateQueue`.
//!
//! The rules under test:
//!
//! * the operation is a PATCH of the queue — only `farmId` and `queueId` are required, so
//!   every member the request carries is applied and every one it leaves out keeps the
//!   value the queue holds, which `GetQueue` reads back;
//! * the two list members are patched, not replaced: the stored list is the one the queue
//!   held with the `…ToRemove` entries taken out and the `…ToAdd` entries it does not
//!   already carry appended, in the order it already held them;
//! * `clientToken` is `@idempotencyToken`, so a retried update is applied once;
//! * a queue is reached only through its own farm, so each missing level of the path is
//!   the modeled 404 naming that level and nothing is written;
//! * removing a storage profile id from `allowedStorageProfileIds` releases the profile:
//!   the `DeleteStorageProfile` that the reference refused succeeds afterwards.
//!
//! The harness (`Emulator`) is the one every other suite in this target drives the service
//! with, so these tests go through routing, deserialization, constraint validation, the
//! handler and serialization, exactly as the router does in production.

use serde_json::{json, Value};

use crate::farm_queue_job::{absent_id, error_code, string_at, Emulator};

/// Every value of the modeled `DefaultQueueBudgetAction`, from
/// `smithy_query(query="shape", shape="DefaultQueueBudgetAction")`: `NONE`,
/// `STOP_SCHEDULING_AND_COMPLETE_TASKS`, `STOP_SCHEDULING_AND_CANCEL_TASKS`.
const BUDGET_ACTIONS: [&str; 3] = [
    "NONE",
    "STOP_SCHEDULING_AND_COMPLETE_TASKS",
    "STOP_SCHEDULING_AND_CANCEL_TASKS",
];

/// An IAM role ARN satisfying the modeled `IamRoleArn` pattern.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/DeadlineQueueUpdated";

fn queue_path(farm_id: &str, queue_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}")
}

/// Create a queue under `farm_id` from `request` and return its id.
async fn create_queue(emulator: &Emulator, farm_id: &str, request: Value) -> String {
    let (status, body) = emulator
        .post(&format!("/2023-10-12/farms/{farm_id}/queues"), request)
        .await;
    assert_eq!(
        status, 200,
        "CreateQueue: expected 200, got {status} with {body}"
    );
    string_at(&body, "queueId")
}

/// `PATCH /2023-10-12/farms/{farmId}/queues/{queueId}`, from
/// `smithy_query(query="operation", operation="UpdateQueue")`.
async fn update_queue(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    body: Value,
    client_token: Option<&str>,
) -> (u16, Value) {
    emulator
        .call(
            "PATCH",
            &queue_path(farm_id, queue_id),
            Some(body),
            client_token,
        )
        .await
}

async fn get_queue(emulator: &Emulator, farm_id: &str, queue_id: &str) -> Value {
    let (status, body) = emulator.get(&queue_path(farm_id, queue_id)).await;
    assert_eq!(
        status, 200,
        "GetQueue: expected 200, got {status} with {body}"
    );
    body
}

#[tokio::test]
async fn update_queue_applies_the_members_it_carries_and_keeps_the_rest() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Patched queue farm").await;

    // The rule: a member the request carries replaces the stored one; a member it omits is
    // untouched. The rows differ along which member moves — each scalar, each enum value of
    // `defaultBudgetAction`, a structure member, and a request that carries none at all.
    let cases: Vec<(&str, Value)> = vec![
        ("no member at all", json!({})),
        ("displayName", json!({"displayName": "Renamed"})),
        ("description", json!({"description": "patched description"})),
        (
            "description emptied",
            json!({"description": ""}),
        ),
        (
            "defaultBudgetAction NONE",
            json!({"defaultBudgetAction": BUDGET_ACTIONS[0]}),
        ),
        (
            "defaultBudgetAction stop and complete",
            json!({"defaultBudgetAction": BUDGET_ACTIONS[1]}),
        ),
        (
            "defaultBudgetAction stop and cancel",
            json!({"defaultBudgetAction": BUDGET_ACTIONS[2]}),
        ),
        ("roleArn", json!({"roleArn": ROLE_ARN})),
        (
            "jobRunAsUser",
            json!({
                "jobRunAsUser": {
                    "runAs": "QUEUE_CONFIGURED_USER",
                    "posix": {"user": "patched", "group": "patchedgroup"},
                },
            }),
        ),
        (
            "every scalar at once",
            json!({
                "displayName": "All at once",
                "description": "all at once",
                "defaultBudgetAction": BUDGET_ACTIONS[1],
                "roleArn": ROLE_ARN,
            }),
        ),
    ];

    for (case, patch) in &cases {
        // Each row patches its own queue, created with the same values, so what the row
        // asserts about the members it did NOT name is about this operation and not about
        // an earlier row.
        let created = json!({
            "displayName": "Before the patch",
            "description": "before the patch",
            "defaultBudgetAction": BUDGET_ACTIONS[0],
            "roleArn": "arn:aws:iam::000000000000:role/DeadlineQueueCreated",
            "requiredFileSystemLocationNames": ["assets"],
            "allowedStorageProfileIds": ["sp-0123456789abcdef0123456789abcdef"],
        });
        let queue_id = create_queue(&emulator, &farm_id, created.clone()).await;

        let (status, body) = update_queue(&emulator, &farm_id, &queue_id, patch.clone(), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateQueue to answer 200, got {status} with {body}"
        );

        let queue = get_queue(&emulator, &farm_id, &queue_id).await;
        for member in [
            "displayName",
            "description",
            "defaultBudgetAction",
            "roleArn",
        ] {
            let expected = patch.get(member).or_else(|| created.get(member));
            assert_eq!(
                queue.get(member),
                expected,
                "case {case:?}: expected {member} to be {expected:?}, got {queue}"
            );
        }
        assert_eq!(
            queue.get("jobRunAsUser"),
            patch.get("jobRunAsUser"),
            "case {case:?}: expected jobRunAsUser to be what the patch named, got {queue}"
        );
        // The lists this row never named are exactly what the create stored.
        for member in ["requiredFileSystemLocationNames", "allowedStorageProfileIds"] {
            assert_eq!(
                queue.get(member),
                created.get(member),
                "case {case:?}: expected the untouched {member}, got {queue}"
            );
        }
        // Every update stamps who changed the queue and when, and leaves the create's own
        // provenance alone.
        assert!(
            queue.get("updatedAt").is_some() && !string_at(&queue, "updatedBy").is_empty(),
            "case {case:?}: expected the update to stamp updatedAt/updatedBy, got {queue}"
        );
        assert!(
            queue.get("createdAt").is_some() && !string_at(&queue, "createdBy").is_empty(),
            "case {case:?}: expected the create's provenance to survive, got {queue}"
        );
    }
}

#[tokio::test]
async fn update_queue_patches_its_list_members_rather_than_replacing_them() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Patched list farm").await;

    // Ids satisfying the modeled `^sp-[0-9a-f]{32}$` pattern, so the request passes the
    // boundary validator and the handler is what decides the outcome.
    let first = absent_id("sp", &format!("{:032x}", 0x31));
    let second = absent_id("sp", &format!("{:032x}", 0x32));
    let third = absent_id("sp", &format!("{:032x}", 0x33));

    // The rule: the stored list is what it held, minus `…ToRemove`, plus the `…ToAdd`
    // entries it does not already carry, in the order it already held them. The rows differ
    // along the operation asked for — add, remove, both, a no-op add of an entry already
    // carried, a removal of an entry never carried, an entry named in both lists, and a
    // removal of every entry.
    let cases: Vec<(&str, Vec<&str>, Value, Vec<&str>)> = vec![
        (
            "add one to an empty list",
            vec![],
            json!({"allowedStorageProfileIdsToAdd": [&first]}),
            vec![&first],
        ),
        (
            "add two to a list of one",
            vec![&first],
            json!({"allowedStorageProfileIdsToAdd": [&second, &third]}),
            vec![&first, &second, &third],
        ),
        (
            "add an entry the list already carries",
            vec![&first, &second],
            json!({"allowedStorageProfileIdsToAdd": [&first]}),
            vec![&first, &second],
        ),
        (
            "remove the middle entry",
            vec![&first, &second, &third],
            json!({"allowedStorageProfileIdsToRemove": [&second]}),
            vec![&first, &third],
        ),
        (
            "remove an entry the list never carried",
            vec![&first],
            json!({"allowedStorageProfileIdsToRemove": [&third]}),
            vec![&first],
        ),
        (
            "add and remove in one request",
            vec![&first, &second],
            json!({
                "allowedStorageProfileIdsToAdd": [&third],
                "allowedStorageProfileIdsToRemove": [&first],
            }),
            vec![&second, &third],
        ),
        (
            "an entry named in both lists stays carried",
            vec![&first],
            json!({
                "allowedStorageProfileIdsToAdd": [&first],
                "allowedStorageProfileIdsToRemove": [&first],
            }),
            vec![&first],
        ),
        (
            "remove every entry",
            vec![&first, &second],
            json!({"allowedStorageProfileIdsToRemove": [&first, &second]}),
            vec![],
        ),
        (
            "empty add and remove lists change nothing",
            vec![&first],
            json!({
                "allowedStorageProfileIdsToAdd": [],
                "allowedStorageProfileIdsToRemove": [],
            }),
            vec![&first],
        ),
    ];

    for (case, before, patch, expected) in &cases {
        let queue_id = create_queue(
            &emulator,
            &farm_id,
            json!({"displayName": format!("List queue {case}"), "allowedStorageProfileIds": before}),
        )
        .await;

        let (status, body) = update_queue(&emulator, &farm_id, &queue_id, patch.clone(), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateQueue to answer 200, got {status} with {body}"
        );
        let queue = get_queue(&emulator, &farm_id, &queue_id).await;
        assert_eq!(
            queue.get("allowedStorageProfileIds"),
            Some(&json!(expected)),
            "case {case:?}: expected allowedStorageProfileIds {expected:?}, got {queue}"
        );
    }

    // The same patching holds for the other list member the model pairs ToAdd/ToRemove on.
    let queue_id = create_queue(
        &emulator,
        &farm_id,
        json!({
            "displayName": "File system locations",
            "requiredFileSystemLocationNames": ["assets", "scratch"],
        }),
    )
    .await;
    let (status, body) = update_queue(
        &emulator,
        &farm_id,
        &queue_id,
        json!({
            "requiredFileSystemLocationNamesToAdd": ["renders"],
            "requiredFileSystemLocationNamesToRemove": ["assets"],
        }),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the file-system-location patch: expected 200, got {status} with {body}"
    );
    let queue = get_queue(&emulator, &farm_id, &queue_id).await;
    assert_eq!(
        queue.get("requiredFileSystemLocationNames"),
        Some(&json!(["scratch", "renders"])),
        "expected requiredFileSystemLocationNames to be patched, got {queue}"
    );
}

#[tokio::test]
async fn a_repeated_client_token_updates_the_queue_once() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Idempotent update farm").await;
    let queue_id = create_queue(
        &emulator,
        &farm_id,
        json!({"displayName": "Retried", "allowedStorageProfileIds": []}),
    )
    .await;
    let added = absent_id("sp", &format!("{:032x}", 0x41));

    // The rule: `clientToken` is `@idempotencyToken`, so the retry of one request is that
    // one request — the add is applied once, not twice, and the second call still answers
    // the success the first did.
    for attempt in 1..=2 {
        let (status, body) = update_queue(
            &emulator,
            &farm_id,
            &queue_id,
            json!({"allowedStorageProfileIdsToAdd": [&added]}),
            Some("update-queue-token"),
        )
        .await;
        assert_eq!(
            status, 200,
            "attempt {attempt}: expected UpdateQueue to answer 200, got {status} with {body}"
        );
    }
    let queue = get_queue(&emulator, &farm_id, &queue_id).await;
    assert_eq!(
        queue.get("allowedStorageProfileIds"),
        Some(&json!([added])),
        "expected the retried add to be applied once, got {queue}"
    );

    // A different token on the same queue is a different update, which applies.
    let (status, body) = update_queue(
        &emulator,
        &farm_id,
        &queue_id,
        json!({"displayName": "Renamed by another token"}),
        Some("update-queue-other-token"),
    )
    .await;
    assert_eq!(
        status, 200,
        "the second token: expected 200, got {status} with {body}"
    );
    let queue = get_queue(&emulator, &farm_id, &queue_id).await;
    assert_eq!(
        string_at(&queue, "displayName"),
        "Renamed by another token",
        "expected a request under a fresh token to apply, got {queue}"
    );
}

#[tokio::test]
async fn a_patch_that_would_pass_the_modeled_list_maximum_is_refused() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Bounded list farm").await;

    // The modeled maximum of both stored list shapes, from
    // `smithy_query(query="shape", shape="AllowedStorageProfileIds")` and
    // `smithy_query(query="shape", shape="RequiredFileSystemLocationNames")`: `len 0..20`.
    const MAXIMUM: usize = 20;
    let ids: Vec<String> = (0..=2 * MAXIMUM)
        .map(|index| absent_id("sp", &format!("{:032x}", 0x100 + index)))
        .collect();

    // The rule: the list a patch leaves the queue with has to fit the maximum the model
    // gives the stored shape, so the rows differ along how full the queue already is and how
    // much the patch would add — up to the maximum, exactly at it, one past it, and an add
    // of entries already held, which adds nothing.
    let cases: Vec<(&str, usize, usize, bool)> = vec![
        ("an empty queue filled to the maximum", 0, MAXIMUM, true),
        ("the last free slot", MAXIMUM - 1, 1, true),
        ("one entry past the maximum", MAXIMUM, 1, false),
        ("a full queue plus one page of additions", MAXIMUM, MAXIMUM, false),
    ];

    for (case, held, added, accepted) in &cases {
        let before: Vec<&String> = ids[..*held].iter().collect();
        let queue_id = create_queue(
            &emulator,
            &farm_id,
            json!({
                "displayName": format!("Bounded {case}"),
                "allowedStorageProfileIds": before,
            }),
        )
        .await;
        // The additions never repeat what the queue holds, so each one is a new entry.
        let to_add: Vec<&String> = ids[*held..*held + *added].iter().collect();

        let (status, body) = update_queue(
            &emulator,
            &farm_id,
            &queue_id,
            json!({"allowedStorageProfileIdsToAdd": to_add}),
            None,
        )
        .await;
        let queue = get_queue(&emulator, &farm_id, &queue_id).await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected UpdateQueue to answer 200, got {status} with {body}"
            );
            assert_eq!(
                queue.get("allowedStorageProfileIds"),
                Some(&json!(ids[..*held + *added])),
                "case {case:?}: expected the patch to be applied, got {queue}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body)),
                (400, "ValidationException".to_string()),
                "case {case:?}: expected the patch to be refused, got {status} with {body}"
            );
            assert_eq!(
                (
                    queue.get("allowedStorageProfileIds"),
                    queue.get("updatedAt")
                ),
                (Some(&json!(before)), None),
                "case {case:?}: expected the refused patch to leave the queue untouched, \
                 got {queue}"
            );
        }
    }

    // The same bound holds for the other list member the model pairs ToAdd/ToRemove on.
    let names: Vec<String> = (0..=MAXIMUM).map(|index| format!("location {index}")).collect();
    let queue_id = create_queue(
        &emulator,
        &farm_id,
        json!({
            "displayName": "Bounded locations",
            "requiredFileSystemLocationNames": &names[..MAXIMUM],
        }),
    )
    .await;
    let (status, body) = update_queue(
        &emulator,
        &farm_id,
        &queue_id,
        json!({"requiredFileSystemLocationNamesToAdd": [&names[MAXIMUM]]}),
        None,
    )
    .await;
    assert_eq!(
        (status, error_code(&body)),
        (400, "ValidationException".to_string()),
        "a 21st file system location name: expected the patch to be refused, got {status} \
         with {body}"
    );
    let queue = get_queue(&emulator, &farm_id, &queue_id).await;
    assert_eq!(
        queue.get("requiredFileSystemLocationNames"),
        Some(&json!(names[..MAXIMUM])),
        "expected the refused patch to leave the locations untouched, got {queue}"
    );
}

#[tokio::test]
async fn updating_a_queue_this_emulator_does_not_hold_is_a_404() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Missing-level farm").await;
    let queue_id = create_queue(&emulator, &farm_id, json!({"displayName": "Held"})).await;
    let absent_farm = absent_id("farm", &format!("{:032x}", 0x51));
    let absent_queue = absent_id("queue", &format!("{:032x}", 0x52));

    // The rule: a queue is reached only through its own farm, so the 404 names the level
    // that is missing — the farm when the farm is unheld, the queue otherwise.
    let cases: Vec<(&str, &str, &str, &str, &str)> = vec![
        (
            "an unheld queue under a held farm",
            &farm_id,
            &absent_queue,
            "queue",
            &absent_queue,
        ),
        (
            "a held queue under an unheld farm",
            &absent_farm,
            &queue_id,
            "farm",
            &absent_farm,
        ),
        (
            "both levels unheld",
            &absent_farm,
            &absent_queue,
            "farm",
            &absent_farm,
        ),
    ];

    for (case, farm, queue, resource_type, resource_id) in &cases {
        let (status, body) = update_queue(
            &emulator,
            farm,
            queue,
            json!({"displayName": "Never applied"}),
            None,
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                (*resource_type).to_string(),
                (*resource_id).to_string(),
            ),
            "case {case:?}: expected a 404 naming the missing level, got {status} with {body}"
        );
    }

    // Nothing was written by any of the refused calls.
    let queue = get_queue(&emulator, &farm_id, &queue_id).await;
    assert_eq!(
        (string_at(&queue, "displayName"), queue.get("updatedAt")),
        ("Held".to_string(), None),
        "expected the held queue to be untouched by the refused updates, got {queue}"
    );
}

#[tokio::test]
async fn removing_a_storage_profile_reference_releases_the_profile_for_deletion() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Released profile farm").await;

    let (status, body) = emulator
        .post(
            &format!("/2023-10-12/farms/{farm_id}/storage-profiles"),
            json!({"displayName": "Referenced", "osFamily": "LINUX"}),
        )
        .await;
    assert_eq!(
        status, 200,
        "CreateStorageProfile: expected 200, got {status} with {body}"
    );
    let storage_profile_id = string_at(&body, "storageProfileId");
    let profile_path = format!("/2023-10-12/farms/{farm_id}/storage-profiles/{storage_profile_id}");
    let queue_id = create_queue(
        &emulator,
        &farm_id,
        json!({
            "displayName": "Holder",
            "allowedStorageProfileIds": [&storage_profile_id],
        }),
    )
    .await;

    // The rule: a queue's reference is what blocks the delete, and `UpdateQueue` is how a
    // caller takes that reference away — so the delete the reference refused succeeds once
    // the update removed it.
    let (status, body) = emulator.call("DELETE", &profile_path, None, None).await;
    assert_eq!(
        (status, error_code(&body)),
        (409, "ConflictException".to_string()),
        "while the queue allows the profile: expected a 409 conflict, got {status} with {body}"
    );

    let (status, body) = update_queue(
        &emulator,
        &farm_id,
        &queue_id,
        json!({"allowedStorageProfileIdsToRemove": [&storage_profile_id]}),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "UpdateQueue removing the reference: expected 200, got {status} with {body}"
    );

    let (status, body) = emulator.call("DELETE", &profile_path, None, None).await;
    assert_eq!(
        status, 200,
        "once the reference is gone: expected the delete to answer 200, got {status} with \
         {body}"
    );
    let (status, read) = emulator.get(&profile_path).await;
    assert_eq!(
        (status, error_code(&read)),
        (404, "ResourceNotFoundException".to_string()),
        "expected the released profile to be gone after the delete, got {status} with {read}"
    );
}
