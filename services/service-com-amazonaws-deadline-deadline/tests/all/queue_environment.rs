// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `CreateQueueEnvironment` and `GetQueueEnvironment`.
//!
//! The rules under test:
//!
//! * a queue environment is stored under the queue its request names and reported back by
//!   the read of the same three ids, carrying the priority, the template type and the
//!   template the create persisted, plus the principal and the instant it stored, and no
//!   update stamps before anything updates it;
//! * its `name` — `@required` on the read, and a member no create takes — is the name the
//!   stored TEMPLATE declares for the environment it describes, read with the parser the
//!   request's `templateType` names, so the same environment described in JSON and in YAML
//!   is reported under the same name;
//! * a template that is not the language it said it was, or that names no environment, is
//!   refused with HTTP 400 `ValidationException` and stores nothing;
//! * `clientToken` is `@idempotencyToken`, so a retry of one create answers with the
//!   environment the first call made instead of making a second one;
//! * an environment is reached only through its own farm and queue, so a create or a read
//!   naming an unheld farm, an unheld queue or another farm's queue stores nothing and
//!   answers a 404 naming the level that is missing.
//!
//! The harness (`Emulator`) is the one the other suites drive the service with, so these
//! tests go through routing, deserialization, constraint validation, the handler and
//! serialization, exactly as the router does in production.

use serde_json::{json, Value};

use crate::farm_queue_job::{
    absent_id, caller, error_code, id_of, is_resource_id, string_at, Emulator,
};
use crate::resource_not_found_wording::not_found_message;

/// The modeled maximum length of an environment template: `EnvironmentTemplate, len
/// 1..15000`, from
/// `smithy_query(query="operation", operation="CreateQueueEnvironment")`.
const MAX_TEMPLATE_LENGTH: usize = 15000;

/// `POST /2023-10-12/farms/{farmId}/queues/{queueId}/environments` — where an environment
/// is created, from
/// `smithy_query(query="operation", operation="CreateQueueEnvironment")`.
fn environments_path(farm_id: &str, queue_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/environments")
}

/// `GET /2023-10-12/farms/{farmId}/queues/{queueId}/environments/{queueEnvironmentId}` —
/// where one environment is read, from
/// `smithy_query(query="operation", operation="GetQueueEnvironment")`.
fn environment_path(farm_id: &str, queue_id: &str, queue_environment_id: &str) -> String {
    format!("{}/{queue_environment_id}", environments_path(farm_id, queue_id))
}

/// An Open Job Description environment template written in JSON.
///
/// The `environment-2023-09` specification describes ONE environment, under the
/// `environment` key, and that environment is what carries the `name` the service reports.
fn json_template(name: &str) -> String {
    json!({
        "specificationVersion": "environment-2023-09",
        "environment": {
            "name": name,
            "variables": {"SEAT": "1"},
        },
    })
    .to_string()
}

/// The same template written in YAML, with the parameter definitions an environment
/// template may declare beside its environment.
fn yaml_template(name: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\n\
         parameterDefinitions:\n\
         - name: SeatCount\n  \
           type: INT\n\
         environment:\n  \
           name: {name}\n  \
           variables:\n    \
             SEAT: '1'\n"
    )
}

/// A JSON template naming `name` whose serialized length is exactly `length` bytes, padded
/// out in the description of the environment it describes.
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

/// Create one environment under `farm_id`/`queue_id`.
async fn create_environment(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    template_type: &str,
    template: &str,
    priority: i64,
    client_token: Option<&str>,
) -> (u16, Value) {
    emulator
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
        .await
}

/// Create one environment and return the id it minted, failing the test if the create was
/// not accepted.
async fn create_environment_ok(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    template_type: &str,
    template: &str,
    priority: i64,
) -> String {
    let (status, body) = create_environment(
        emulator,
        farm_id,
        queue_id,
        template_type,
        template,
        priority,
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateQueueEnvironment ({template_type}): expected 200, got {status} with {body}"
    );
    id_of(&body, "queueEnvironmentId")
}

/// Read one environment back, failing the test if it can't be read.
async fn read_environment(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    queue_environment_id: &str,
) -> Value {
    let (status, body) = emulator
        .get(&environment_path(farm_id, queue_id, queue_environment_id))
        .await;
    assert_eq!(
        status, 200,
        "GetQueueEnvironment {queue_environment_id}: expected 200, got {status} with {body}"
    );
    body
}

/// How many environments the store holds under `farm_id`/`queue_id`.
///
/// Read straight from this emulator's store, because no implemented operation lists them
/// yet: it is what makes "the refused create stored nothing" an assertion about state
/// rather than about the refusal alone.
fn stored_environment_count(emulator: &Emulator, farm_id: &str, queue_id: &str) -> usize {
    let store = emulator.store();
    let held = store.lock();
    held.queue(farm_id, queue_id)
        .map_or(0, |queue| queue.environments.len())
}

#[tokio::test]
async fn create_queue_environment_then_get_reports_what_was_created() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Environment farm").await;
    let queue_id = emulator.queue(&farm_id, "environments").await;

    // The rule: the read reports the values the create persisted, with the name taken from
    // the template. The rows differ along the template's language, the name it declares,
    // the priority (both published bounds of `Priority, range 0..10000` and a value
    // between them) and the template's size (a small document and one at the modeled
    // `EnvironmentTemplate, len 1..15000` maximum).
    let cases: Vec<(&str, &str, String, &str, i64)> = vec![
        (
            "a JSON template at the lowest priority",
            "JSON",
            json_template("Blender"),
            "Blender",
            0,
        ),
        (
            "a YAML template at the highest priority",
            "YAML",
            yaml_template("Maya"),
            "Maya",
            10000,
        ),
        (
            "a YAML template at a middling priority",
            "YAML",
            yaml_template("Nuke"),
            "Nuke",
            5000,
        ),
        (
            "a JSON template at its modeled length maximum",
            "JSON",
            json_template_of_length("Houdini", MAX_TEMPLATE_LENGTH),
            "Houdini",
            1,
        ),
    ];

    let mut created: Vec<(&str, String, String)> = Vec::new();
    for (case, template_type, template, name, priority) in &cases {
        let queue_environment_id = create_environment_ok(
            &emulator,
            &farm_id,
            &queue_id,
            template_type,
            template,
            *priority,
        )
        .await;
        assert!(
            is_resource_id(&queue_environment_id, "queueenv"),
            "case {case:?}: expected a minted queueenv id, got {queue_environment_id:?}"
        );

        let body = read_environment(&emulator, &farm_id, &queue_id, &queue_environment_id).await;
        assert_eq!(
            (
                string_at(&body, "queueEnvironmentId"),
                string_at(&body, "name"),
                body.get("priority").and_then(Value::as_i64),
                string_at(&body, "templateType"),
                string_at(&body, "template"),
                string_at(&body, "createdBy"),
            ),
            (
                queue_environment_id.clone(),
                (*name).to_string(),
                Some(*priority),
                (*template_type).to_string(),
                template.clone(),
                caller(),
            ),
            "case {case:?}: expected the stored environment, got {body}"
        );
        assert!(
            // Modelled `@timestampFormat("date-time")`: an RFC 3339 string, never the
            // protocol-default epoch number (`date_time_wire_form` owns that rule).
            body.get("createdAt").and_then(Value::as_str).is_some(),
            "case {case:?}: expected createdAt as an RFC 3339 string, got {body}"
        );
        assert!(
            body.get("updatedAt").is_none() && body.get("updatedBy").is_none(),
            "case {case:?}: expected no updatedAt/updatedBy before an update, got {body}"
        );

        // A read does not change what the next reader sees.
        let again = read_environment(&emulator, &farm_id, &queue_id, &queue_environment_id).await;
        assert_eq!(
            again, body,
            "case {case:?}: a second read answered differently, got {again}"
        );

        created.push((case, queue_environment_id, (*name).to_string()));
    }

    // Each create added an environment of its own: a later one neither replaced an earlier
    // environment nor took its id.
    assert_eq!(
        stored_environment_count(&emulator, &farm_id, &queue_id),
        cases.len(),
        "expected one stored environment per create"
    );
    for (case, queue_environment_id, name) in &created {
        let body = read_environment(&emulator, &farm_id, &queue_id, queue_environment_id).await;
        assert_eq!(
            string_at(&body, "name"),
            *name,
            "case {case:?}: expected every created environment to still be held, got {body}"
        );
    }
}

#[tokio::test]
async fn the_same_environment_in_json_and_in_yaml_is_named_the_same() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Language farm").await;
    let queue_id = emulator.queue(&farm_id, "two languages").await;

    // The rule: the name comes out of the template in the language `templateType` names, so
    // one environment described in either language is reported under the one name. The rows
    // differ along the name the template declares, including one carrying the characters a
    // YAML scalar is quoted for.
    for name in ["Blender", "Houdini 19.5", "with: colon"] {
        let from_json = create_environment_ok(
            &emulator,
            &farm_id,
            &queue_id,
            "JSON",
            &json_template(name),
            7,
        )
        .await;
        let quoted = format!("'{}'", name.replace('\'', "''"));
        let from_yaml = create_environment_ok(
            &emulator,
            &farm_id,
            &queue_id,
            "YAML",
            &yaml_template(&quoted),
            7,
        )
        .await;

        let json_name = string_at(
            &read_environment(&emulator, &farm_id, &queue_id, &from_json).await,
            "name",
        );
        let yaml_name = string_at(
            &read_environment(&emulator, &farm_id, &queue_id, &from_yaml).await,
            "name",
        );
        assert_eq!(
            (json_name.as_str(), yaml_name.as_str()),
            (name, name),
            "case {name:?}: expected both languages to name the environment {name:?}, got \
             {json_name:?} and {yaml_name:?}"
        );
    }
}

#[tokio::test]
async fn a_template_the_service_cannot_name_an_environment_from_is_refused() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Refusing farm").await;
    let queue_id = emulator.queue(&farm_id, "refusals").await;

    // The rule: a template that is not the language it said it was, or that names no
    // environment, describes no environment this service could report, so the create is
    // refused and stores nothing. The rows differ along the language and along which of the
    // two faults the document carries.
    let cases: Vec<(&str, &str, String, &str)> = vec![
        (
            "a JSON template that isn't JSON",
            "JSON",
            "{\"environment\": {\"name\":".to_string(),
            "CANNOT_PARSE",
        ),
        (
            "a YAML template that isn't YAML",
            "YAML",
            "environment:\n  name: 'unterminated\n".to_string(),
            "CANNOT_PARSE",
        ),
        (
            "a JSON template that names no environment",
            "JSON",
            json!({"specificationVersion": "environment-2023-09"}).to_string(),
            "FIELD_VALIDATION_FAILED",
        ),
        (
            "a YAML template that names no environment",
            "YAML",
            "specificationVersion: environment-2023-09\nenvironment:\n  variables: {}\n"
                .to_string(),
            "FIELD_VALIDATION_FAILED",
        ),
        (
            "a JSON template whose environment name is empty",
            "JSON",
            json_template(""),
            "FIELD_VALIDATION_FAILED",
        ),
        (
            "a JSON template whose environment name is not a string",
            "JSON",
            json!({
                "specificationVersion": "environment-2023-09",
                "environment": {"name": 7},
            })
            .to_string(),
            "FIELD_VALIDATION_FAILED",
        ),
    ];

    for (case, template_type, template, reason) in &cases {
        let (status, body) = create_environment(
            &emulator,
            &farm_id,
            &queue_id,
            template_type,
            template,
            3,
            None,
        )
        .await;
        assert_eq!(
            (status, error_code(&body), string_at(&body, "reason")),
            (
                400,
                "ValidationException".to_string(),
                (*reason).to_string()
            ),
            "case {case:?}: expected a 400 ValidationException, got {status} with {body}"
        );
        assert!(
            body.get("queueEnvironmentId").is_none(),
            "case {case:?}: expected the refusal to mint no id, got {body}"
        );
        assert_eq!(
            stored_environment_count(&emulator, &farm_id, &queue_id),
            0,
            "case {case:?}: expected the refused create to store nothing"
        );
    }

    // The same request with a template that does name an environment is accepted, so what
    // the rows above are refused for is the template and nothing else.
    let accepted =
        create_environment_ok(&emulator, &farm_id, &queue_id, "JSON", &json_template("Arnold"), 3)
            .await;
    assert_eq!(
        string_at(
            &read_environment(&emulator, &farm_id, &queue_id, &accepted).await,
            "name"
        ),
        "Arnold",
        "after the refusals: expected a well-formed template to be accepted"
    );
}

#[tokio::test]
async fn a_yaml_template_past_this_emulators_parse_bounds_is_refused() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Bounded farm").await;
    let queue_id = emulator.queue(&farm_id, "bounds").await;

    // The rule: a YAML document is read within bounds taken BEFORE a tree is built from it
    // — the text's nesting, and the collections and aliases its parse reports — because a
    // loader resolves an alias by copying the anchored node, so what a document expands to
    // is not bounded by how long it is. Past any of them the create is refused with the
    // modelled 400 and stores nothing. The rows differ along which bound the document
    // passes: the text scan's nesting, the parse's open-collection depth, and the alias it
    // uses to expand.
    let deep_mapping: String = (0..40)
        .map(|level| format!("{}k{level}:\n", " ".repeat(level)))
        .collect();
    let cases: Vec<(&str, String)> = vec![
        (
            "a document nesting past the text scan's bound",
            format!("{}1{}", "[".repeat(300), "]".repeat(300)),
        ),
        (
            "a document whose parse opens more collections than the bound",
            deep_mapping,
        ),
        (
            "a document that expands through an anchor and an alias",
            "specificationVersion: environment-2023-09\n\
             environment: &env\n  \
               name: Blender\n\
             copy: *env\n"
                .to_string(),
        ),
        (
            "a document that chains aliases, the billion-laughs shape",
            "a: &a [x, x]\nb: &b [*a, *a]\nc: &c [*b, *b]\nenvironment:\n  name: Blender\n"
                .to_string(),
        ),
    ];

    for (case, template) in &cases {
        let (status, body) =
            create_environment(&emulator, &farm_id, &queue_id, "YAML", template, 2, None).await;
        assert_eq!(
            (status, error_code(&body), string_at(&body, "reason")),
            (
                400,
                "ValidationException".to_string(),
                "CANNOT_PARSE".to_string()
            ),
            "case {case:?}: expected the document to be refused unparseable, got {status} \
             with {body}"
        );
        assert_eq!(
            stored_environment_count(&emulator, &farm_id, &queue_id),
            0,
            "case {case:?}: expected the refused create to store nothing"
        );
    }

    // A document INSIDE every bound is read: the guards refuse expansion, not nesting a
    // template legitimately carries.
    let nested = format!(
        "specificationVersion: environment-2023-09\n\
         environment:\n  \
           name: Blender\n  \
           script:\n    \
             actions:\n      \
               onEnter:\n        \
                 command: {}\n",
        "x".repeat(16)
    );
    let accepted =
        create_environment_ok(&emulator, &farm_id, &queue_id, "YAML", &nested, 2).await;
    assert_eq!(
        string_at(
            &read_environment(&emulator, &farm_id, &queue_id, &accepted).await,
            "name"
        ),
        "Blender",
        "a document inside every bound: expected it to be read and stored"
    );
}

#[tokio::test]
async fn a_retried_create_answers_with_the_environment_the_first_call_made() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Idempotent farm").await;
    let queue_id = emulator.queue(&farm_id, "retried").await;

    // The rule: `clientToken` is `@idempotencyToken`, so every call carrying one token
    // answers with the one environment it created. The rows differ along how many times the
    // same token is replayed and along the token itself, which is what separates one create
    // from the next.
    let mut seen: Vec<String> = Vec::new();
    for (case, token, repeats) in [
        ("a token replayed once", "token-first", 1),
        ("a token replayed twice", "token-second", 2),
        ("a token of maximum modeled length", &"t".repeat(64), 1),
    ] {
        let (status, body) = create_environment(
            &emulator,
            &farm_id,
            &queue_id,
            "JSON",
            &json_template(case),
            9,
            Some(token),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first create to answer 200, got {status} with {body}"
        );
        let first = id_of(&body, "queueEnvironmentId");
        let stored = read_environment(&emulator, &farm_id, &queue_id, &first).await;

        for repeat in 0..repeats {
            let (status, body) = create_environment(
                &emulator,
                &farm_id,
                &queue_id,
                "JSON",
                // A retry carries the same body; what identifies it is the token.
                &json_template(case),
                9,
                Some(token),
            )
            .await;
            assert_eq!(
                (status, id_of(&body, "queueEnvironmentId")),
                (200, first.clone()),
                "case {case:?} retry {repeat}: expected the first environment's id, got \
                 {status} with {body}"
            );
            let again = read_environment(&emulator, &farm_id, &queue_id, &first).await;
            assert_eq!(
                again, stored,
                "case {case:?} retry {repeat}: expected the stored environment to be \
                 untouched, got {again}"
            );
        }

        // A different token is a different create, so the ids do not collide.
        assert!(
            !seen.contains(&first),
            "case {case:?}: expected a fresh id for a fresh token, got {first}"
        );
        seen.push(first);
    }

    assert_eq!(
        stored_environment_count(&emulator, &farm_id, &queue_id),
        seen.len(),
        "expected one stored environment per distinct client token"
    );
}

#[tokio::test]
async fn an_environment_is_reached_only_through_its_own_farm_and_queue() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("Owning farm").await;
    let other_farm_id = emulator.farm("Stranger farm").await;
    let queue_id = emulator.queue(&farm_id, "owning queue").await;
    let other_queue_id = emulator.queue(&farm_id, "sibling queue").await;
    let other_farm_queue = emulator.queue(&other_farm_id, "elsewhere").await;
    let environment_id = create_environment_ok(
        &emulator,
        &farm_id,
        &queue_id,
        "JSON",
        &json_template("Owned"),
        4,
    )
    .await;

    let absent_farm = absent_id("farm", &format!("{:032x}", 0x51));
    let absent_queue = absent_id("queue", &format!("{:032x}", 0x52));
    let absent_environment = absent_id("queueenv", &format!("{:032x}", 0x53));

    // The rule: an environment lives under a queue of one farm, so a CREATE naming an
    // unheld farm, an unheld queue or another farm's queue stores nothing and answers a 404
    // naming the level that is missing. The rows differ along which level that is.
    let creates: Vec<(&str, &String, &String, &str, &String)> = vec![
        (
            "an unheld farm",
            &absent_farm,
            &queue_id,
            "farm",
            &absent_farm,
        ),
        (
            "an unheld queue",
            &farm_id,
            &absent_queue,
            "queue",
            &absent_queue,
        ),
        (
            "a queue of another farm",
            &farm_id,
            &other_farm_queue,
            "queue",
            &other_farm_queue,
        ),
    ];

    for (case, farm, queue, resource_type, resource_id) in &creates {
        let (status, body) = create_environment(
            &emulator,
            farm,
            queue,
            "JSON",
            &json_template("Unstored"),
            4,
            None,
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "message"),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                not_found_message(resource_type, resource_id),
                (*resource_type).to_string(),
                (*resource_id).clone(),
            ),
            "case {case:?}: expected the create's 404 to name what is missing, got {status} \
             with {body}"
        );
    }

    // And the same for a READ: every level of the path is reported as itself, and the
    // environment's own id is what is missing when the farm and the queue are held.
    let reads: Vec<(&str, &String, &String, &String, &str, &String)> = vec![
        (
            "an unheld farm",
            &absent_farm,
            &queue_id,
            &environment_id,
            "farm",
            &absent_farm,
        ),
        (
            "an unheld queue",
            &farm_id,
            &absent_queue,
            &environment_id,
            "queue",
            &absent_queue,
        ),
        (
            "an unheld environment",
            &farm_id,
            &queue_id,
            &absent_environment,
            "environment",
            &absent_environment,
        ),
        (
            "an environment of another queue of the same farm",
            &farm_id,
            &other_queue_id,
            &environment_id,
            "environment",
            &environment_id,
        ),
        (
            "an environment of another farm's queue",
            &other_farm_id,
            &other_farm_queue,
            &environment_id,
            "environment",
            &environment_id,
        ),
    ];

    for (case, farm, queue, environment, resource_type, resource_id) in &reads {
        let (status, body) = emulator.get(&environment_path(farm, queue, environment)).await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "message"),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                not_found_message(resource_type, resource_id),
                (*resource_type).to_string(),
                (*resource_id).clone(),
            ),
            "case {case:?}: expected the read's 404 to name what is missing, got {status} \
             with {body}"
        );
    }

    // None of those refusals disturbed the one environment this test did create.
    assert_eq!(
        string_at(
            &read_environment(&emulator, &farm_id, &queue_id, &environment_id).await,
            "name"
        ),
        "Owned",
        "after the refusals: expected the held environment to be untouched"
    );
    assert_eq!(
        stored_environment_count(&emulator, &farm_id, &queue_id),
        1,
        "after the refusals: expected the queue to hold exactly the one environment"
    );
}
