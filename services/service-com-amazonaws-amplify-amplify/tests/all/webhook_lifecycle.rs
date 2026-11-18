// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the Amplify webhook lifecycle.
//!
//! Every case drives the real wire path (the shared harness in
//! `tests/all/app_branch_lifecycle.rs`), so what is asserted is the JSON an AWS SDK
//! client parses, not an internal function's return value.
//!
//! The rule under test: `CreateWebhook` mints a webhook under the app its URI names,
//! PERSISTS it, and answers the stored record — so the webhook is readable afterwards
//! by its own id and comes back field-for-field, with the identity the service minted
//! (`webhookId`, and the `webhookArn`/`webhookUrl` built from it), the `branchName`
//! and `description` the request carried, and `createTime` equal to `updateTime` on a
//! record nothing has updated. A webhook belongs to its app: an unknown `appId` is
//! refused with the modelled 404 and stores nothing, and deleting the app takes its
//! webhooks with it.
//!
//! The table spans the class the rule generalises over — `description` absent, empty,
//! single-line, multi-line and at the modelled `length` maximum of 1000; `branchName`
//! at one character, at a path-like name, and at the modelled maximum of 255; a branch
//! that exists on the app and one that does not — rather than one recorded request.
//!
//! `DeleteWebhook` closes the same lifecycle: a delete addressed by `webhookId` TAKES
//! that record out of the app holding it and answers the record as it stood, so every
//! later reader of the same collection (`GetWebhook`, `ListWebhooks`, the app's webhook
//! quota) no longer sees it, every other webhook is left exactly as it was, and an id
//! the store does not hold — one no create minted, or one an earlier delete already took
//! out — is the modelled 404.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::app_branch_lifecycle::{
    string_member, timestamp_member, Emulator, Reply, ACCOUNT_ID, REGION,
};

/// `DeleteWebhook` driven over the wire, which the model binds to
/// `DELETE /webhooks/{webhookId}`
/// (`smithy_query(query="operation",operation="DeleteWebhook")`).
async fn delete_webhook(emulator: &Emulator, webhook_id: &str) -> Reply {
    emulator
        .call("DELETE", &format!("/webhooks/{webhook_id}"), None)
        .await
}

/// The ids `ListWebhooks` reports for `app_id`, read with no page bound.
async fn listed_webhook_ids(emulator: &Emulator, app_id: &str) -> Vec<String> {
    let listed = emulator.list_webhooks(app_id, "").await;
    assert_eq!(
        listed.status, 200,
        "expected HTTP 200 from ListWebhooks for {app_id}, got {listed:?}"
    );
    listed
        .member("webhooks")
        .as_array()
        .unwrap_or_else(|| panic!("ListWebhooks must carry a list, got {listed:?}"))
        .iter()
        .map(|record| string_member(record, "webhookId"))
        .collect()
}

/// Assert that `webhook_id` names nothing this store holds: the read and a further
/// delete are both the modelled 404, each naming the entity the request addressed.
async fn assert_absent(label: &str, emulator: &Emulator, webhook_id: &str) {
    for (operation, reply) in [
        ("GetWebhook", emulator.get_webhook(webhook_id).await),
        ("DeleteWebhook", delete_webhook(emulator, webhook_id).await),
    ] {
        assert_eq!(
            reply.status, 404,
            "case {label:?}: expected HTTP 404 from {operation} for an id the store does not \
             hold, got {reply:?}"
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("NotFoundException"),
            "case {label:?}: expected NotFoundException from {operation}, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.message(),
            Some(format!("Webhook {webhook_id} not found.")),
            "case {label:?}: the 404 from {operation} names the entity the request addressed, \
             got {:?}",
            reply.message()
        );
    }
}

/// Every member modelled on `com.amazonaws.amplify#Webhook`, as the wire names
/// `smithy_query(query="shape",shape="Webhook")` lists them.
const MODELLED_WEBHOOK_MEMBERS: [&str; 8] = [
    "appId",
    "branchName",
    "createTime",
    "description",
    "updateTime",
    "webhookArn",
    "webhookId",
    "webhookUrl",
];

/// `WebhookId`'s modelled `length` maximum (`length 0..255`, from
/// `smithy_query(query="shape",shape="Webhook")`).
const WEBHOOK_ID_MAX_LENGTH: usize = 255;

/// Assert that `record` is a COMPLETE `Webhook`: every modelled member present, and
/// nothing the shape does not model.
fn assert_complete_webhook(label: &str, operation: &str, record: &Value) {
    let members = record
        .as_object()
        .unwrap_or_else(|| panic!("case {label:?}: {operation} must answer a webhook object"));
    for member in MODELLED_WEBHOOK_MEMBERS {
        assert!(
            members.contains_key(member),
            "case {label:?}: {operation} must carry the modelled member {member:?}, got \
             {record:?}"
        );
    }
    for member in members.keys() {
        assert!(
            MODELLED_WEBHOOK_MEMBERS.contains(&member.as_str()),
            "case {label:?}: {operation} carries {member:?}, which Webhook does not model, got \
             {record:?}"
        );
    }
}

/// One `CreateWebhook` request plus the record the rule says it must produce.
///
/// `branch_name` and `description` are the WIRE values expected for those members, so
/// a request that omits `description` still expects the member: it is `@required` with
/// a `length` minimum of 0, so an omitted description is the empty one.
struct WebhookCase {
    case: &'static str,
    request: Value,
    branch_name: String,
    description: String,
}

/// The `CreateWebhook` table.
///
/// It varies `description` over absent, empty, single-line, multi-line (the modelled
/// `pattern ^(?s)` admits newlines) and the modelled `length` maximum of 1000, and
/// `branchName` over one character (its modelled `length` minimum), an ordinary name,
/// a path-like name and the modelled maximum of 255 — plus a branch the app does not
/// have, because a webhook is scoped to its APP and its `branchName` is the name its
/// trigger targets. The bounds come from
/// `smithy_query(query="operation",operation="CreateWebhook")`.
fn webhook_cases() -> Vec<WebhookCase> {
    vec![
        WebhookCase {
            case: "only the required branchName",
            request: json!({ "branchName": "main" }),
            branch_name: "main".to_string(),
            description: String::new(),
        },
        WebhookCase {
            case: "description at the modelled length minimum (the empty string)",
            request: json!({ "branchName": "main", "description": "" }),
            branch_name: "main".to_string(),
            description: String::new(),
        },
        WebhookCase {
            case: "a described webhook on a path-like branch name",
            request: json!({
                "branchName": "feature/add-login",
                "description": "triggers the login preview build",
            }),
            branch_name: "feature/add-login".to_string(),
            description: "triggers the login preview build".to_string(),
        },
        WebhookCase {
            case: "a multi-line description",
            request: json!({
                "branchName": "release-1.0",
                "description": "nightly trigger\nowned by the release team",
            }),
            branch_name: "release-1.0".to_string(),
            description: "nightly trigger\nowned by the release team".to_string(),
        },
        WebhookCase {
            case: "description at the modelled length maximum of 1000",
            request: json!({ "branchName": "main", "description": "d".repeat(1000) }),
            branch_name: "main".to_string(),
            description: "d".repeat(1000),
        },
        WebhookCase {
            case: "branchName of one character, its modelled length minimum",
            request: json!({ "branchName": "x", "description": "single-character branch" }),
            branch_name: "x".to_string(),
            description: "single-character branch".to_string(),
        },
        WebhookCase {
            case: "branchName at the modelled length maximum of 255",
            request: json!({ "branchName": "b".repeat(255) }),
            branch_name: "b".repeat(255),
            description: String::new(),
        },
        WebhookCase {
            case: "a branch name the app has no branch for",
            request: json!({ "branchName": "not-created-yet", "description": "future branch" }),
            branch_name: "not-created-yet".to_string(),
            description: "future branch".to_string(),
        },
    ]
}

/// Assert everything the rule says about one created webhook, and return its id.
fn assert_created(label: &str, app_id: &str, case: &WebhookCase, created: &Reply) -> String {
    assert_eq!(
        created.status, 200,
        "case {label:?}: expected HTTP 200 from CreateWebhook, got {created:?}"
    );
    let record = created.member("webhook");
    assert_complete_webhook(label, "CreateWebhook", &record);

    let webhook_id = string_member(&record, "webhookId");
    assert!(
        !webhook_id.is_empty() && webhook_id.len() <= WEBHOOK_ID_MAX_LENGTH,
        "case {label:?}: webhookId must be a non-empty value inside the modelled length \
         maximum of {WEBHOOK_ID_MAX_LENGTH}, got {webhook_id:?}"
    );
    assert_eq!(
        string_member(&record, "appId"),
        app_id,
        "case {label:?}: the webhook belongs to the app its URI named, got {record:?}"
    );
    assert_eq!(
        string_member(&record, "branchName"),
        case.branch_name,
        "case {label:?}: branchName must be echoed as the request set it, got {record:?}"
    );
    assert_eq!(
        string_member(&record, "description"),
        case.description,
        "case {label:?}: description must be the one the request carried, got {record:?}"
    );
    assert_eq!(
        string_member(&record, "webhookArn"),
        format!("arn:aws:amplify:{REGION}:{ACCOUNT_ID}:apps/{app_id}/webhooks/{webhook_id}"),
        "case {label:?}: the ARN names the app and the minted id, got {record:?}"
    );

    // The URL is the trigger endpoint for THIS webhook, so it carries the minted id;
    // nothing about it is derivable from the request alone.
    let url = string_member(&record, "webhookUrl");
    assert!(
        url.starts_with("https://") && url.contains(&webhook_id),
        "case {label:?}: webhookUrl must be an https URL carrying the webhook id, got {url:?}"
    );

    let create_time = timestamp_member(&record, "createTime");
    assert!(
        create_time > 0.0,
        "case {label:?}: createTime must be a real instant, got {create_time}"
    );
    assert_eq!(
        timestamp_member(&record, "updateTime"),
        create_time,
        "case {label:?}: a webhook nothing has updated has updateTime equal to createTime, got \
         {record:?}"
    );

    webhook_id
}

#[tokio::test]
async fn a_created_webhook_is_readable_by_its_id() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("webhook-host").await;
    // A second app exists throughout: a webhook created on one app must never change
    // what the other app's webhook reads as.
    let other_app_id = emulator.app_id_of_new_app("webhook-bystander").await;
    let branch = emulator
        .create_branch(&app_id, &json!({ "branchName": "main" }))
        .await;
    assert_eq!(
        branch.status, 200,
        "expected HTTP 200 from the CreateBranch that the webhooks target, got {branch:?}"
    );

    let bystander = emulator
        .create_webhook(&other_app_id, &json!({ "branchName": "main" }))
        .await;
    assert_eq!(
        bystander.status, 200,
        "expected HTTP 200 for the bystander CreateWebhook, got {bystander:?}"
    );
    let bystander_id = string_member(&bystander.member("webhook"), "webhookId");
    let bystander_record = bystander.member("webhook");

    let mut minted_ids = BTreeSet::new();
    minted_ids.insert(bystander_id.clone());

    for case in webhook_cases() {
        let label = case.case;
        let created = emulator.create_webhook(&app_id, &case.request).await;
        let webhook_id = assert_created(label, &app_id, &case, &created);
        let record = created.member("webhook");

        assert!(
            minted_ids.insert(webhook_id.clone()),
            "case {label:?}: every create mints an id no other webhook has, got \
             {webhook_id:?} twice"
        );

        // The created record is PERSISTED: the read answers it field-for-field.
        let fetched = emulator.get_webhook(&webhook_id).await;
        assert_eq!(
            fetched.status, 200,
            "case {label:?}: expected HTTP 200 from GetWebhook, got {fetched:?}"
        );
        let fetched_record = fetched.member("webhook");
        assert_complete_webhook(label, "GetWebhook", &fetched_record);
        assert_eq!(
            fetched_record, record,
            "case {label:?}: GetWebhook must answer the created record field-for-field, got \
             {fetched_record:?}"
        );

        // A read changes nothing a later caller sees.
        assert_eq!(
            emulator.get_webhook(&webhook_id).await.member("webhook"),
            fetched_record,
            "case {label:?}: reading a webhook must not change what a later caller sees"
        );
        assert_eq!(
            emulator.get_webhook(&bystander_id).await.member("webhook"),
            bystander_record,
            "case {label:?}: creating a webhook must not touch another app's webhook"
        );
    }
}

#[tokio::test]
async fn repeated_identical_creates_mint_distinct_webhooks() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("webhook-repeat-host").await;
    let request = json!({ "branchName": "main", "description": "same request every time" });

    // A webhook's identity is MINTED, never derived from the request: the same request
    // sent five times is five webhooks, each readable on its own.
    let mut ids = BTreeSet::new();
    let mut urls = BTreeSet::new();
    for attempt in 0..5 {
        let created = emulator.create_webhook(&app_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case \"repeat {attempt}\": expected HTTP 200 from CreateWebhook, got {created:?}"
        );
        let record = created.member("webhook");
        let webhook_id = string_member(&record, "webhookId");
        assert!(
            ids.insert(webhook_id.clone()),
            "case \"repeat {attempt}\": expected a webhookId no earlier create minted, got \
             {webhook_id:?}"
        );
        assert!(
            urls.insert(string_member(&record, "webhookUrl")),
            "case \"repeat {attempt}\": expected a webhookUrl no earlier create minted, got \
             {record:?}"
        );
        assert_eq!(
            emulator.get_webhook(&webhook_id).await.member("webhook"),
            record,
            "case \"repeat {attempt}\": each minted webhook is readable on its own"
        );
    }
}

#[tokio::test]
async fn a_webhook_under_an_absent_app_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("webhook-refusal-host").await;

    // `AppId` is modelled `length 1..20, pattern ^d[a-z0-9]+$`, so each of these is a
    // well-formed id for an app that does not exist — the request passes the boundary
    // validator and the refusal is the handler's.
    for absent_app in ["dnosuchapp1", "d0", &format!("d{}", "9".repeat(18))] {
        let refused = emulator
            .create_webhook(absent_app, &json!({ "branchName": "main" }))
            .await;
        assert_eq!(
            refused.status, 404,
            "case {absent_app:?}: expected HTTP 404 from CreateWebhook, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("NotFoundException"),
            "case {absent_app:?}: expected NotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            Some(format!("App {absent_app} not found.")),
            "case {absent_app:?}: the 404 names the entity the request addressed, got {:?}",
            refused.message()
        );
    }

    // An id no webhook has is the same modelled refusal on the read side.
    for absent_id in ["no-such-webhook", "00000000-0000-4000-8000-000000000000"] {
        let refused = emulator.get_webhook(absent_id).await;
        assert_eq!(
            refused.status, 404,
            "case {absent_id:?}: expected HTTP 404 from GetWebhook, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("NotFoundException"),
            "case {absent_id:?}: expected NotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            Some(format!("Webhook {absent_id} not found.")),
            "case {absent_id:?}: the 404 names the entity the request addressed, got {:?}",
            refused.message()
        );
    }

    // The refusals stored nothing: the real app still has no webhook, so the one
    // created now is the first id this app hands out.
    let created = emulator
        .create_webhook(&app_id, &json!({ "branchName": "main" }))
        .await;
    assert_eq!(
        created.status, 200,
        "expected HTTP 200 from CreateWebhook after the refusals, got {created:?}"
    );
}

#[tokio::test]
async fn webhooks_are_removed_with_the_app_that_owns_them() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("webhook-deleted-host").await;
    let survivor_app_id = emulator.app_id_of_new_app("webhook-surviving-host").await;

    let mut ids = Vec::new();
    for branch in ["main", "develop", "release"] {
        let created = emulator
            .create_webhook(&app_id, &json!({ "branchName": branch }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {branch:?}: expected HTTP 200 from CreateWebhook, got {created:?}"
        );
        ids.push(string_member(&created.member("webhook"), "webhookId"));
    }
    let survivor = emulator
        .create_webhook(&survivor_app_id, &json!({ "branchName": "main" }))
        .await;
    assert_eq!(
        survivor.status, 200,
        "expected HTTP 200 for the surviving app's CreateWebhook, got {survivor:?}"
    );
    let survivor_id = string_member(&survivor.member("webhook"), "webhookId");
    let survivor_record = survivor.member("webhook");

    let deleted = emulator.delete_app(&app_id).await;
    assert_eq!(
        deleted.status, 200,
        "expected HTTP 200 from DeleteApp, got {deleted:?}"
    );

    for webhook_id in &ids {
        let gone = emulator.get_webhook(webhook_id).await;
        assert_eq!(
            gone.status, 404,
            "case {webhook_id:?}: a webhook of a deleted app is gone, got {gone:?}"
        );
    }
    assert_eq!(
        emulator.get_webhook(&survivor_id).await.member("webhook"),
        survivor_record,
        "deleting one app must leave another app's webhook exactly as it was"
    );
}

#[tokio::test]
async fn delete_webhook_answers_the_stored_record_and_takes_it_out_of_state() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("webhook-delete-host").await;
    // A second app holds a webhook throughout: deleting one app's webhook must never
    // change what another app's webhook reads as.
    let other_app_id = emulator.app_id_of_new_app("webhook-delete-bystander").await;
    let bystander = emulator
        .create_webhook(&other_app_id, &json!({ "branchName": "main" }))
        .await;
    assert_eq!(
        bystander.status, 200,
        "expected HTTP 200 for the bystander CreateWebhook, got {bystander:?}"
    );
    let bystander_id = string_member(&bystander.member("webhook"), "webhookId");
    let bystander_record = bystander.member("webhook");

    // Every row of the create table is created first, so each delete removes ONE
    // webhook from a collection that holds several rather than the only one there is.
    let mut pending: Vec<(&'static str, String, Value)> = Vec::new();
    for case in webhook_cases() {
        let created = emulator.create_webhook(&app_id, &case.request).await;
        let webhook_id = assert_created(case.case, &app_id, &case, &created);
        pending.push((case.case, webhook_id, created.member("webhook")));
    }

    while let Some((label, webhook_id, record)) = pending.pop() {
        let deleted = delete_webhook(&emulator, &webhook_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected HTTP 200 from DeleteWebhook, got {deleted:?}"
        );
        let deleted_record = deleted.member("webhook");
        assert_complete_webhook(label, "DeleteWebhook", &deleted_record);
        assert_eq!(
            deleted_record, record,
            "case {label:?}: DeleteWebhook must answer the record as it stood, got \
             {deleted_record:?}"
        );

        // The record is OUT of state: the read is the modelled 404, and so is a second
        // delete of the same id.
        assert_absent(label, &emulator, &webhook_id).await;

        // Only the addressed webhook went: everything not yet deleted is still listed
        // under the app, in the collection the delete removed one element from.
        let remaining = listed_webhook_ids(&emulator, &app_id).await;
        let mut expected: Vec<String> = pending.iter().map(|(_, id, _)| id.clone()).collect();
        expected.sort();
        let mut listed = remaining.clone();
        listed.sort();
        assert_eq!(
            listed, expected,
            "case {label:?}: the app must list exactly the webhooks no delete has taken, got \
             {remaining:?}"
        );
        for (other_label, other_id, other_record) in &pending {
            assert_eq!(
                emulator.get_webhook(other_id).await.member("webhook"),
                *other_record,
                "case {label:?}: deleting a webhook must leave {other_label:?} exactly as it was"
            );
        }
        assert_eq!(
            emulator.get_webhook(&bystander_id).await.member("webhook"),
            bystander_record,
            "case {label:?}: deleting a webhook must not touch another app's webhook"
        );
    }

    assert!(
        listed_webhook_ids(&emulator, &app_id).await.is_empty(),
        "once every webhook is deleted the app lists none, got {:?}",
        listed_webhook_ids(&emulator, &app_id).await
    );
}

#[tokio::test]
async fn a_delete_reports_the_record_as_the_last_write_left_it() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("webhook-delete-after-update").await;

    // The update table spans what `UpdateWebhook` can change: the branch alone, the
    // description alone, and both at once.
    for (label, update) in [
        ("branchName alone", json!({ "branchName": "develop" })),
        ("description alone", json!({ "description": "re-described" })),
        (
            "both members at once",
            json!({ "branchName": "release-2.0", "description": "both changed" }),
        ),
    ] {
        let created = emulator
            .create_webhook(&app_id, &json!({ "branchName": "main" }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected HTTP 200 from CreateWebhook, got {created:?}"
        );
        let webhook_id = string_member(&created.member("webhook"), "webhookId");

        let updated = emulator.update_webhook(&webhook_id, &update).await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: expected HTTP 200 from UpdateWebhook, got {updated:?}"
        );
        let updated_record = updated.member("webhook");

        let deleted = delete_webhook(&emulator, &webhook_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected HTTP 200 from DeleteWebhook, got {deleted:?}"
        );
        assert_eq!(
            deleted.member("webhook"),
            updated_record,
            "case {label:?}: the deleted record is the one the last write left, got {:?}",
            deleted.member("webhook")
        );
        assert_absent(label, &emulator, &webhook_id).await;
    }
}

#[tokio::test]
async fn a_delete_of_an_id_the_store_does_not_hold_is_refused_and_removes_nothing() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("webhook-delete-refusal-host").await;
    let kept = emulator
        .create_webhook(&app_id, &json!({ "branchName": "main", "description": "kept" }))
        .await;
    assert_eq!(
        kept.status, 200,
        "expected HTTP 200 from the CreateWebhook that must survive the refusals, got {kept:?}"
    );
    let kept_id = string_member(&kept.member("webhook"), "webhookId");
    let kept_record = kept.member("webhook");

    // An id of a webhook whose APP was deleted: the record went with its app, so the
    // id resolves to nothing even though a create once minted it.
    let doomed_app_id = emulator.app_id_of_new_app("webhook-delete-doomed-host").await;
    let doomed = emulator
        .create_webhook(&doomed_app_id, &json!({ "branchName": "main" }))
        .await;
    assert_eq!(
        doomed.status, 200,
        "expected HTTP 200 from the CreateWebhook on the app about to be deleted, got {doomed:?}"
    );
    let doomed_id = string_member(&doomed.member("webhook"), "webhookId");
    assert_eq!(
        emulator.delete_app(&doomed_app_id).await.status,
        200,
        "expected HTTP 200 from DeleteApp"
    );

    // `WebhookId` is modelled `length 0..255, pattern ^(?s)`
    // (`smithy_query(query="shape",shape="Webhook")`), so each of these passes the
    // boundary validator and the refusal is the handler's: a value the service never
    // mints, a well-formed UUID no create handed out, one at the modelled length
    // maximum, and the id of a webhook that no longer exists.
    let absent_ids = [
        "no-such-webhook".to_string(),
        "00000000-0000-4000-8000-000000000000".to_string(),
        "w".repeat(WEBHOOK_ID_MAX_LENGTH),
        doomed_id,
    ];
    for absent_id in &absent_ids {
        assert_absent("an id the store does not hold", &emulator, absent_id).await;
    }

    // The refusals removed nothing: the webhook that does exist is untouched, and it
    // is still the only one its app lists.
    assert_eq!(
        emulator.get_webhook(&kept_id).await.member("webhook"),
        kept_record,
        "a refused delete must leave every stored webhook exactly as it was"
    );
    assert_eq!(
        listed_webhook_ids(&emulator, &app_id).await,
        vec![kept_id],
        "a refused delete must leave the app's webhook collection as it was"
    );
}

#[tokio::test]
async fn a_delete_frees_the_room_the_deleted_webhook_held_under_its_app() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("webhook-delete-quota-host").await;

    // Create until the app's webhook quota refuses one more, so the test derives the
    // bound from the service instead of restating it. The attempt ceiling is generous:
    // it only stops a runaway loop if the service were to refuse nothing.
    let mut ids = Vec::new();
    let refusal = loop {
        let created = emulator
            .create_webhook(&app_id, &json!({ "branchName": "main" }))
            .await;
        if created.status != 200 {
            break created;
        }
        ids.push(string_member(&created.member("webhook"), "webhookId"));
        assert!(
            ids.len() <= 1_000,
            "case \"filling the quota\": the app accepted {} webhooks without refusing one",
            ids.len()
        );
    };
    assert_eq!(
        refusal.status, 429,
        "case \"at the quota\": expected HTTP 429 once the app is full, got {refusal:?}"
    );
    assert_eq!(
        refusal.error_code.as_deref(),
        Some("LimitExceededException"),
        "case \"at the quota\": expected LimitExceededException, got {:?}",
        refusal.error_code
    );
    let held = ids.len();
    assert_eq!(
        listed_webhook_ids(&emulator, &app_id).await.len(),
        held,
        "case \"at the quota\": the app lists every webhook it accepted"
    );

    // Deleting one makes room for exactly one more, because the delete took the record
    // out of the very collection the quota counts.
    let removed = ids.remove(0);
    assert_eq!(
        delete_webhook(&emulator, &removed).await.status,
        200,
        "case \"freeing room\": expected HTTP 200 from DeleteWebhook"
    );
    let replacement = emulator
        .create_webhook(&app_id, &json!({ "branchName": "main", "description": "replacement" }))
        .await;
    assert_eq!(
        replacement.status, 200,
        "case \"freeing room\": a create must succeed in the room the delete freed, got \
         {replacement:?}"
    );
    let replacement_id = string_member(&replacement.member("webhook"), "webhookId");
    assert_ne!(
        replacement_id, removed,
        "case \"freeing room\": the replacement is a newly minted webhook, not the deleted id"
    );
    assert_eq!(
        listed_webhook_ids(&emulator, &app_id).await.len(),
        held,
        "case \"freeing room\": the app holds as many webhooks as the quota allows"
    );
    let full_again = emulator
        .create_webhook(&app_id, &json!({ "branchName": "main" }))
        .await;
    assert_eq!(
        full_again.status, 429,
        "case \"full again\": one delete frees room for one create, got {full_again:?}"
    );
}
