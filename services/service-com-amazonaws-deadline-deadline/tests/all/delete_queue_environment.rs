// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteQueueEnvironment`.
//!
//! The rules under test:
//!
//! * a delete removes the environment from the queue that holds it, so the read that
//!   follows it answers the modeled 404 and a repeat of the delete is still an empty
//!   success — the operation is `@idempotent` and models no not-found error
//!   (`smithy_query(query="operation", operation="DeleteQueueEnvironment")` lists only
//!   `AccessDeniedException`, `InternalServerErrorException`, `ThrottlingException` and
//!   `ValidationException`), so an environment this emulator no longer holds — and a queue
//!   or a farm it never held — is nothing left to remove rather than a refusal;
//! * an environment is reached only through its own farm's queue, so a delete addressing it
//!   through another queue's path removes nothing and leaves it readable;
//! * what the removed environment held goes back to the store: the template bytes it was
//!   charged for at create time, and the client token of the create that made it, so
//!   replaying that token after the delete creates a new environment instead of answering
//!   with an id nothing holds.
//!
//! The harness (`Emulator`) is the one every other suite in this target drives the service
//! with, so these tests go through routing, deserialization, constraint validation, the
//! handler and serialization, exactly as the router does in production.

use serde_json::{json, Value};

use crate::farm_queue_job::{absent_id, error_code, string_at, Emulator};
use crate::resource_not_found_wording::not_found_message;

/// The modeled maximum length of an environment template: `EnvironmentTemplate, len
/// 1..15000`, from
/// `smithy_query(query="operation", operation="CreateQueueEnvironment")`.
const MAX_TEMPLATE_LENGTH: usize = 15000;

/// `resourceType` a 404 reports for a queue environment: the kebab-case URI segment,
/// singular — `/environments/` names an `environment`
/// (see `crate::resource_not_found_wording`).
const RESOURCE_TYPE_ENVIRONMENT: &str = "environment";

/// How many bytes of template text this emulator holds across every stored resource,
/// re-exported from the service's own state so this suite names the one budget rather than
/// a second copy of the number.
const MAX_STORED_TEMPLATE_BYTES: usize =
    service_com_amazonaws_deadline_deadline::state::MAX_STORED_TEMPLATE_BYTES;

fn environments_path(farm_id: &str, queue_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/environments")
}

fn environment_path(farm_id: &str, queue_id: &str, queue_environment_id: &str) -> String {
    format!(
        "{}/{queue_environment_id}",
        environments_path(farm_id, queue_id)
    )
}

/// An Open Job Description environment template written in JSON, naming the environment it
/// describes.
fn json_template(name: &str) -> String {
    json!({
        "specificationVersion": "environment-2023-09",
        "environment": {"name": name, "variables": {"SEAT": "1"}},
    })
    .to_string()
}

/// The same environment written in YAML.
fn yaml_template(name: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\n\
         environment:\n  \
           name: {name}\n  \
           variables:\n    \
             SEAT: '1'\n"
    )
}

/// A JSON template naming `name` whose serialized length is exactly `length` bytes.
fn json_template_of_length(name: &str, length: usize) -> String {
    let template = |padding: usize| {
        json!({
            "specificationVersion": "environment-2023-09",
            "environment": {"name": name, "description": "d".repeat(padding)},
        })
        .to_string()
    };
    let padding = length.saturating_sub(template(0).len());
    let padded = template(padding);
    assert_eq!(
        padded.len(),
        length,
        "a padded template of {length} bytes: got {} bytes",
        padded.len()
    );
    padded
}

/// Create one environment under `farm_id`/`queue_id` and return its id.
async fn create_environment(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    template_type: &str,
    template: &str,
    priority: i64,
    client_token: Option<&str>,
) -> String {
    let (status, body) = emulator
        .call(
            "POST",
            &environments_path(farm_id, queue_id),
            Some(json!({
                "priority": priority,
                "templateType": template_type,
                "template": template,
            })),
            client_token,
        )
        .await;
    assert_eq!(
        status, 200,
        "CreateQueueEnvironment ({template_type}): expected 200, got {status} with {body}"
    );
    string_at(&body, "queueEnvironmentId")
}

async fn delete_environment(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    queue_environment_id: &str,
) -> (u16, Value) {
    emulator
        .call(
            "DELETE",
            &environment_path(farm_id, queue_id, queue_environment_id),
            None,
            None,
        )
        .await
}

/// Is the whole template byte budget free — nothing stored holding any of it?
///
/// `template_bytes_fit` is the store's own question ("would this many more bytes fit?"), so
/// asking it for the WHOLE budget answers true exactly when no stored resource is charged
/// for a single byte.
fn template_budget_is_free(emulator: &Emulator) -> bool {
    let store = emulator.store();
    let held = store.lock();
    held.template_bytes_fit(MAX_STORED_TEMPLATE_BYTES)
}

/// How many environments the store holds under `farm_id`/`queue_id`.
fn stored_environment_count(emulator: &Emulator, farm_id: &str, queue_id: &str) -> usize {
    let store = emulator.store();
    let held = store.lock();
    held.queue(farm_id, queue_id)
        .map_or(0, |queue| queue.environments.len())
}

#[tokio::test]
async fn deleting_a_queue_environment_removes_it_and_repeats_as_an_empty_success() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Deleting farm").await;
    let queue_id = emulator.queue(&farm_id, "environments").await;

    // The rule: the delete removes the environment from its queue, whatever that
    // environment holds. The rows differ along the template's language, the name it
    // declares, the priority (both published bounds of `Priority, range 0..10000` and a
    // value between them) and the template's size (a small document and one at the modeled
    // `EnvironmentTemplate, len 1..15000` maximum).
    let cases: Vec<(&str, &str, String, i64)> = vec![
        (
            "a JSON template at the lowest priority",
            "JSON",
            json_template("Blender"),
            0,
        ),
        (
            "a YAML template at the highest priority",
            "YAML",
            yaml_template("Maya"),
            10000,
        ),
        (
            "a YAML template at a middling priority",
            "YAML",
            yaml_template("Nuke"),
            5000,
        ),
        (
            "a JSON template at its modeled length maximum",
            "JSON",
            json_template_of_length("Houdini", MAX_TEMPLATE_LENGTH),
            1,
        ),
    ];

    for (case, template_type, template, priority) in &cases {
        let queue_environment_id = create_environment(
            &emulator,
            &farm_id,
            &queue_id,
            template_type,
            template,
            *priority,
            None,
        )
        .await;

        let (status, body) =
            delete_environment(&emulator, &farm_id, &queue_id, &queue_environment_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteQueueEnvironment to answer 200, got {status} with \
             {body}"
        );
        assert_eq!(
            stored_environment_count(&emulator, &farm_id, &queue_id),
            0,
            "case {case:?}: expected the queue to hold no environment after the delete"
        );

        let (status, read) = emulator
            .get(&environment_path(&farm_id, &queue_id, &queue_environment_id))
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the deleted environment to be gone, got {status} with \
             {read}"
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
                RESOURCE_TYPE_ENVIRONMENT.to_string(),
                queue_environment_id.clone(),
                not_found_message(RESOURCE_TYPE_ENVIRONMENT, &queue_environment_id),
            ),
            "case {case:?}: expected the 404 to name the deleted environment, got {read}"
        );

        // `@idempotent`, and the model declares no not-found error: a repeat of the delete
        // has nothing left to remove and answers the same empty success.
        let (status, body) =
            delete_environment(&emulator, &farm_id, &queue_id, &queue_environment_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a repeated delete to answer 200, got {status} with \
             {body}"
        );
        assert_eq!(
            body,
            Value::Object(serde_json::Map::new()),
            "case {case:?}: expected an empty success body, got {body}"
        );
    }
}

#[tokio::test]
async fn deleting_a_queue_environment_this_emulator_does_not_hold_removes_nothing() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Idempotent delete farm").await;
    let queue_id = emulator.queue(&farm_id, "holder").await;
    let other_queue_id = emulator.queue(&farm_id, "other holder").await;
    let other_farm_id = emulator.farm("Other farm").await;
    let other_farms_queue_id = emulator.queue(&other_farm_id, "elsewhere").await;

    let kept = create_environment(
        &emulator,
        &farm_id,
        &queue_id,
        "JSON",
        &json_template("Kept"),
        50,
        None,
    )
    .await;
    // 32 hex characters, as the modeled `^farm-[0-9a-f]{32}$` / `^queue-[0-9a-f]{32}$` /
    // `^queueenv-[0-9a-f]{32}$` label patterns require, so the request passes the boundary
    // validator and the handler is what decides the outcome.
    let absent_farm = absent_id("farm", &format!("{:032x}", 0x21));
    let absent_queue = absent_id("queue", &format!("{:032x}", 0x22));
    let absent_environment = absent_id("queueenv", &format!("{:032x}", 0x23));

    // The rule: with no not-found error modeled, a well-formed id naming nothing is an
    // empty success, and an environment is reached only through its own farm's queue. The
    // rows differ along which level of the path misses — the environment, the queue, the
    // farm, every one of them — and along the two ways a path reaches the WRONG holder: a
    // sibling queue of the same farm, and another farm's queue. None of them touches the
    // environment the queue does hold.
    let cases: Vec<(&str, &str, &str, &str)> = vec![
        (
            "an unheld environment under the holding queue",
            &farm_id,
            &queue_id,
            &absent_environment,
        ),
        (
            "the held environment under an unheld queue",
            &farm_id,
            &absent_queue,
            &kept,
        ),
        (
            "the held environment under an unheld farm",
            &absent_farm,
            &queue_id,
            &kept,
        ),
        (
            "the held environment under a sibling queue of the same farm",
            &farm_id,
            &other_queue_id,
            &kept,
        ),
        (
            "the held environment under another farm's queue",
            &other_farm_id,
            &other_farms_queue_id,
            &kept,
        ),
        (
            "no level of the path held",
            &absent_farm,
            &absent_queue,
            &absent_environment,
        ),
    ];

    for (case, farm, queue, environment) in &cases {
        let (status, body) = delete_environment(&emulator, farm, queue, environment).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteQueueEnvironment to answer 200, got {status} with \
             {body}"
        );

        let (status, read) = emulator
            .get(&environment_path(&farm_id, &queue_id, &kept))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the held environment to stay readable, got {status} \
             with {read}"
        );
        assert_eq!(
            string_at(&read, "name"),
            "Kept",
            "case {case:?}: expected the held environment to be untouched, got {read}"
        );
        assert_eq!(
            stored_environment_count(&emulator, &farm_id, &queue_id),
            1,
            "case {case:?}: expected the holding queue to still hold its one environment"
        );
    }
}

#[tokio::test]
async fn deleting_a_queue_environment_releases_its_template_bytes() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Budget farm").await;
    let queue_id = emulator.queue(&farm_id, "budget").await;

    // The rule: an environment's template is charged to the store's shared byte budget
    // while it is stored and released when it is deleted, so a budget spent by environments
    // that no longer exist is a budget no later create can use. The rows differ along the
    // template's size, from a small document to one at the modeled maximum.
    assert!(
        template_budget_is_free(&emulator),
        "before any create: expected the whole template budget to be free"
    );

    let mut created: Vec<(&str, String)> = Vec::new();
    for (case, template) in [
        ("a small template", json_template("Small")),
        (
            "a template at its modeled length maximum",
            json_template_of_length("Large", MAX_TEMPLATE_LENGTH),
        ),
    ] {
        created.push((
            case,
            create_environment(&emulator, &farm_id, &queue_id, "JSON", &template, 25, None).await,
        ));
        assert!(
            !template_budget_is_free(&emulator),
            "case {case:?}: expected the stored template to hold part of the budget"
        );
    }

    for (case, queue_environment_id) in &created {
        let (status, body) =
            delete_environment(&emulator, &farm_id, &queue_id, queue_environment_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteQueueEnvironment to answer 200, got {status} with \
             {body}"
        );
    }
    assert!(
        template_budget_is_free(&emulator),
        "after deleting every environment: expected the whole template budget to be free \
         again"
    );
}

#[tokio::test]
async fn deleting_a_queue_environment_releases_its_client_token() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Token farm").await;
    let queue_id = emulator.queue(&farm_id, "tokens").await;

    // The rule: a client token remembers a resource only while that resource exists, so
    // replaying the token after the delete creates a new environment rather than answering
    // with an id nothing holds. The rows differ along the template's language, since the
    // token is what identifies the retry, not the document.
    for (case, template_type, template) in [
        ("a JSON template", "JSON", json_template("Replayed JSON")),
        ("a YAML template", "YAML", yaml_template("ReplayedYaml")),
    ] {
        let token = format!("token-release-{template_type}");
        let first = create_environment(
            &emulator,
            &farm_id,
            &queue_id,
            template_type,
            &template,
            10,
            Some(&token),
        )
        .await;

        // While it exists, the same token answers with the environment the first call made.
        let retried = create_environment(
            &emulator,
            &farm_id,
            &queue_id,
            template_type,
            &template,
            10,
            Some(&token),
        )
        .await;
        assert_eq!(
            retried, first,
            "case {case:?}: expected the replayed token to answer with the first environment"
        );

        let (status, body) = delete_environment(&emulator, &farm_id, &queue_id, &first).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteQueueEnvironment to answer 200, got {status} with \
             {body}"
        );

        let second = create_environment(
            &emulator,
            &farm_id,
            &queue_id,
            template_type,
            &template,
            10,
            Some(&token),
        )
        .await;
        assert_ne!(
            second, first,
            "case {case:?}: expected a new environment after the first was deleted, got \
             {first} twice"
        );
        let (status, read) = emulator
            .get(&environment_path(&farm_id, &queue_id, &second))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the recreated environment to be readable, got {status} \
             with {read}"
        );

        let (status, body) = delete_environment(&emulator, &farm_id, &queue_id, &second).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the cleanup delete to answer 200, got {status} with \
             {body}"
        );
    }
}
