// SPDX-License-Identifier: CC0-1.0

//! The form a backend environment name a CREATE brings into being has to take.
//!
//! Every case drives the real wire path (the shared harness in
//! `tests/all/app_branch_lifecycle.rs`), so what is asserted is the JSON an AWS SDK
//! client parses.
//!
//! The rule under test: `CreateBackendEnvironment` brings a Gen 1 backend environment
//! into being only under a name of two to ten characters, every one of them a lowercase
//! letter. A name outside that form is refused with the operation's own modelled client
//! error — HTTP 400 `BadRequestException` — whose sentence is fixed apart from the
//! rejected name, which it quotes verbatim from the request, and nothing is stored — so
//! the name the request asked for is readable nowhere afterwards and the app's listing is
//! unchanged. The bound is on the name a create brings into being: a name a request
//! merely ADDRESSES is not held to it, so a read or a delete of a name outside the form
//! answers the same `NotFoundException` as any other environment this store does not hold
//! (`backend_environment_absence_wording`).
//!
//! `EnvironmentName` is modelled `length 1..255, pattern ^(?s).+$`
//! (`smithy_query(query="operation",operation="CreateBackendEnvironment")`), which admits
//! every value here, so the form is the service's own rule and not a modelled constraint:
//! the refusals below reach the handler rather than the boundary validator.
//!
//! The tables span the class the rule generalises over: each published bound of the
//! length window, one character past each of them, and every character class outside the
//! alphabet (upper case, digits, punctuation, whitespace, non-ASCII letters) — none of
//! them the name any one recorded run generated.

use serde_json::{json, Value};

use crate::app_branch_lifecycle::{string_member, Emulator, Reply};

/// The error code a name outside the form is refused with: one of the operation's own
/// modelled errors (`BadRequestException`, `InternalFailureException`,
/// `LimitExceededException`, `NotFoundException`, `UnauthorizedException`, from
/// `smithy_query(query="operation",operation="CreateBackendEnvironment")`), and the one
/// AWS answers a malformed `environmentName` with.
const BAD_REQUEST_EXCEPTION: &str = "BadRequestException";

/// The sentence the refusal states for `name`: fixed text apart from the rejected name,
/// which is quoted verbatim from the request (including the service's own missing space
/// after the first sentence).
fn name_rule_message(name: &str) -> String {
    format!(
        "Failed to create backend environment.The environment name {name} is invalid. \
         Environment name should be between 2 and 10 characters (only lowercase alphabets)."
    )
}

/// `CreateBackendEnvironment` over the wire.
async fn create_environment(emulator: &Emulator, app_id: &str, request: &Value) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/apps/{app_id}/backendenvironments"),
            Some(request),
        )
        .await
}

/// `GetBackendEnvironment` over the wire.
async fn get_environment(emulator: &Emulator, app_id: &str, environment_name: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/apps/{app_id}/backendenvironments/{environment_name}"),
            None,
        )
        .await
}

/// The `environmentName`s `ListBackendEnvironments` reports for an app, in page order.
async fn listed_names(emulator: &Emulator, app_id: &str) -> Vec<String> {
    let reply = emulator
        .call("GET", &format!("/apps/{app_id}/backendenvironments"), None)
        .await;
    assert_eq!(
        reply.status, 200,
        "app {app_id}: expected HTTP 200 from ListBackendEnvironments, got {reply:?}"
    );
    reply
        .body
        .get("backendEnvironments")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("app {app_id}: ListBackendEnvironments must carry a list, got {reply:?}")
        })
        .iter()
        .map(|record| string_member(record, "environmentName"))
        .collect()
}

/// Whether `name` can go in a URI path as written, so a case can read the name back
/// through `GetBackendEnvironment`. A name carrying whitespace or a non-ASCII letter
/// would have to be percent-encoded, which would put a DIFFERENT name on the wire.
fn addressable_in_a_path(name: &str) -> bool {
    name.chars()
        .all(|character| character.is_ascii_alphanumeric() || "-._~".contains(character))
}

/// The names the rule REFUSES: each published bound of the length window overshot by one
/// character, and one row per character class outside the lowercase-letter alphabet.
fn refused_name_cases() -> Vec<(&'static str, String)> {
    vec![
        ("one character, one below the length minimum", "e".to_string()),
        (
            "eleven characters, one above the length maximum",
            "e".repeat(11),
        ),
        (
            "far above the length maximum",
            "stagingenvironment".to_string(),
        ),
        ("a leading upper-case letter", "Staging".to_string()),
        ("upper case throughout", "STAGING".to_string()),
        ("a trailing digit", "staging1".to_string()),
        ("digits only", "20260101".to_string()),
        ("a hyphen", "my-env".to_string()),
        ("an underscore", "my_env".to_string()),
        ("a dot", "my.env".to_string()),
        ("an inner space", "my env".to_string()),
        ("non-ASCII lower-case letters", "staging\u{e9}".to_string()),
        (
            "several classes at once: case, digits and a hyphen",
            "stg-7F2".to_string(),
        ),
    ]
}

/// The names the rule ACCEPTS: both published bounds of the length window and ordinary
/// names between them.
fn accepted_name_cases() -> Vec<(&'static str, String)> {
    vec![
        ("two characters, the length minimum", "qa".to_string()),
        ("an ordinary name", "staging".to_string()),
        ("ten characters, the length maximum", "e".repeat(10)),
        ("every letter distinct", "abcdefghij".to_string()),
    ]
}

#[tokio::test]
async fn a_create_under_a_name_outside_the_form_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("backend-env-name-rule").await;

    // One environment exists throughout: a refused create may not disturb it.
    let keeper = create_environment(&emulator, &app_id, &json!({ "environmentName": "keeper" }))
        .await;
    assert_eq!(
        keeper.status, 200,
        "expected HTTP 200 seeding the environment that must survive, got {keeper:?}"
    );
    let keeper_record = keeper.member("backendEnvironment");

    for (label, name) in refused_name_cases() {
        // The optional members are supplied on half the rows: the refusal is about the
        // name, so what else the request carries cannot change it.
        let request = if name.len() % 2 == 0 {
            json!({ "environmentName": &name })
        } else {
            json!({
                "environmentName": &name,
                "stackName": "amplify-demo-stack",
                "deploymentArtifacts": "amplify-demo-deployment",
            })
        };
        let refused = create_environment(&emulator, &app_id, &request).await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: expected HTTP 400 for a name outside the form, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(BAD_REQUEST_EXCEPTION),
            "case {label:?}: expected the error code {BAD_REQUEST_EXCEPTION:?}, got {:?}",
            refused.error_code
        );
        let expected_message = name_rule_message(&name);
        assert_eq!(
            refused.message(),
            Some(expected_message.clone()),
            "case {label:?}: expected the refusal to state {expected_message:?}, got {:?}",
            refused.message()
        );

        // Nothing was stored under the name the request asked for.
        if addressable_in_a_path(&name) {
            let absent = get_environment(&emulator, &app_id, &name).await;
            assert_eq!(
                absent.status, 404,
                "case {label:?}: a refused create must store nothing, got {absent:?}"
            );
            assert_eq!(
                absent.message(),
                Some(format!("BackendEnvironment {name} does not exist.")),
                "case {label:?}: the read of the refused name is the ordinary absence refusal, \
                 got {:?}",
                absent.message()
            );
        }
    }

    // Every refusal left the app exactly as it was.
    assert_eq!(
        listed_names(&emulator, &app_id).await,
        vec!["keeper".to_string()],
        "a refused create must leave the app's listing alone"
    );
    assert_eq!(
        get_environment(&emulator, &app_id, "keeper")
            .await
            .member("backendEnvironment"),
        keeper_record,
        "a refused create must leave the stored environment alone"
    );
}

#[tokio::test]
async fn the_refusal_quotes_the_name_the_request_sent() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("backend-env-name-quoted").await;

    // The sentence's only variable part is the rejected name, so no two names that break
    // the rule can be refused with the same sentence, and each one carries its own name.
    let mut seen: Vec<String> = Vec::new();
    for (label, name) in refused_name_cases() {
        let refused =
            create_environment(&emulator, &app_id, &json!({ "environmentName": &name })).await;
        let message = refused.message().unwrap_or_else(|| {
            panic!("case {label:?}: the refusal must carry a message, got {refused:?}")
        });
        assert!(
            message.contains(&name),
            "case {label:?}: the refusal must quote the rejected name {name:?}, got {message:?}"
        );
        assert!(
            !seen.contains(&message),
            "case {label:?}: the sentence is templated on the name, so it must differ from \
             every other refused name's, got {message:?}"
        );
        seen.push(message);
    }

    // Nothing the refusals named reached the app.
    assert!(
        listed_names(&emulator, &app_id).await.is_empty(),
        "refused creates must store nothing, got {:?}",
        listed_names(&emulator, &app_id).await
    );
}

#[tokio::test]
async fn a_create_under_a_name_inside_the_form_is_served_and_reads_back() {
    let emulator = Emulator::new();
    let app_id = emulator.app_id_of_new_app("backend-env-name-accepted").await;

    for (label, name) in accepted_name_cases() {
        let created =
            create_environment(&emulator, &app_id, &json!({ "environmentName": &name })).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected HTTP 200 from CreateBackendEnvironment, got {created:?}"
        );
        let record = created.member("backendEnvironment");
        assert_eq!(
            string_member(&record, "environmentName"),
            name,
            "case {label:?}: the created record carries the name the request sent, got {record:?}"
        );

        // The accepted name really reached state: the reader answers the created record.
        let fetched = get_environment(&emulator, &app_id, &name).await;
        assert_eq!(
            fetched.status, 200,
            "case {label:?}: an accepted name must be readable, got {fetched:?}"
        );
        assert_eq!(
            fetched.member("backendEnvironment"),
            record,
            "case {label:?}: GetBackendEnvironment must answer the created record, got {fetched:?}"
        );
    }

    let mut expected: Vec<String> = accepted_name_cases()
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    expected.sort();
    assert_eq!(
        listed_names(&emulator, &app_id).await,
        expected,
        "every accepted name must be listed by the app that holds it"
    );
}
