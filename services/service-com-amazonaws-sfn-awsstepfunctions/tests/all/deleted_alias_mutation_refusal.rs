// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how the alias MUTATION path words the refusal of an alias
//! record that is no longer held, driven over this service's own wire protocol
//! (`awsJson1_0`, `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`) through
//! the production `MultiServiceRouter`, so what is asserted is what a signed AWS SDK
//! request observes in `$.message`.
//!
//! The rule under test: an alias ARN resolves in two steps — the state machine it
//! qualifies, then the alias name inside it — and once the ALIAS RECORD alone is gone
//! from a state machine the service still holds, `UpdateStateMachineAlias` refuses the
//! request with `ResourceNotFound` (HTTP 400 for this awsJson1_0 service) worded as ONE
//! fixed sentence that carries no value of the request: not the alias ARN, not the
//! alias name, not the state machine. The refused ARN travels in the shape's
//! `resourceName` member instead.
//!
//! What the rule does NOT move, asserted here as its boundary:
//!
//! * the alias READ path (`DescribeStateMachineAlias`) quotes the ARN the request
//!   supplied — the two wordings are both required and must not be unified;
//! * `DeleteStateMachineAlias` of an alias that is already gone is a success, because
//!   the post-condition the caller asked for already holds, so the sentence belongs to
//!   the UPDATE alone;
//! * neither refusal writes anything: repeating the update answers exactly the same,
//!   and the state machine's surviving aliases are still updatable afterwards.
//!
//! The harness (`Emulator`, `error_code`, `string`) is the one
//! `state_machine_lifecycle` builds, `failure` is `error_messages`' own, the published
//! versions come from `state_machine_versions`' `machine_with_versions` and the alias
//! create and its routing entries from `state_machine_aliases`; `tests/all/main.rs` is
//! this binary's crate root, so each is reached through `crate::`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error_messages::failure;
use crate::state_machine_aliases::{create_alias, route};
use crate::state_machine_lifecycle::{error_code, string, Emulator};
use crate::state_machine_versions::machine_with_versions;

/// The sentence the alias MUTATION path answers for an alias record it resolved
/// nothing for, written out here rather than read off the implementation, so an edit
/// to the crate's own constant cannot keep this test green. It is wholly fixed text.
const ALIAS_GONE_SENTENCE: &str = "Request references a resource that does not exist.";

/// The status every client refusal of this service reads, whatever the model's
/// `@httpError` says for the error shape.
const CLIENT_ERROR_STATUS: u16 = 400;

/// How long a row waits for a started delete to have taken its alias away. Generous on
/// purpose: what is asserted is the WORDING of the refusal that follows, never how fast
/// the host reached it, so this is only a backstop against a delete that never lands.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long a row waits between two looks at the deleted alias.
const BETWEEN_LOOKS: Duration = Duration::from_millis(25);

/// Which members an update names.
///
/// "You must specify at least one of the description or routingConfiguration
/// parameters to update a state machine alias" is the operation's own documented rule,
/// so a row names one, the other, or both — and the refusal below is the same sentence
/// however the request is shaped.
#[derive(Clone, Copy, Debug)]
enum Names {
    Description,
    Routing,
    Both,
}

/// One row of the table: the case, how many versions its state machine publishes, the
/// name of the alias that is deleted, and the members the update names.
struct Row {
    case: &'static str,
    versions: u32,
    alias: &'static str,
    names: Names,
}

/// The rows differ along the member the update names, the number of routing entries it
/// carries, how the weights are split, the alias name and the state machine behind it
/// — none of which may appear in the fixed sentence, which is what shows nothing is
/// interpolated into it.
fn rows() -> Vec<Row> {
    vec![
        // The recorded row: a weighted re-point of an alias that was deleted, the
        // blue/green deploy's own call.
        Row {
            case: "a one-version alias, repointed at its only version",
            versions: 1,
            alias: "PROD",
            names: Names::Routing,
        },
        Row {
            case: "a two-version alias, repointed across both versions",
            versions: 2,
            alias: "blue-green.1",
            names: Names::Routing,
        },
        Row {
            case: "an alias re-described and nothing else",
            versions: 1,
            alias: "weighted_rollout-2",
            names: Names::Description,
        },
        Row {
            case: "an alias re-described AND repointed in one call",
            versions: 2,
            alias: "Canary.Stage-3_east",
            names: Names::Both,
        },
    ]
}

/// The routing configuration a row re-points its alias with: the only version when its
/// state machine published one, and a weighted split across both when it published two.
fn routing_of(machine: &str, versions: u32) -> Vec<Value> {
    if versions < 2 {
        vec![route(&format!("{machine}:1"), 100)]
    } else {
        vec![
            route(&format!("{machine}:1"), 70),
            route(&format!("{machine}:2"), 30),
        ]
    }
}

/// The `UpdateStateMachineAlias` request body for `alias_arn`, naming the members
/// `names` says it names.
fn update_request(alias_arn: &str, names: Names, routing: &[Value]) -> Value {
    let mut request = json!({ "stateMachineAliasArn": alias_arn });
    if matches!(names, Names::Description | Names::Both) {
        request["description"] = json!("re-pointed by the rollout");
    }
    if matches!(names, Names::Routing | Names::Both) {
        request["routingConfiguration"] = json!(routing);
    }
    request
}

/// `UpdateStateMachineAlias` that must succeed, and the `updateDate` it reported.
async fn updated(emulator: &Emulator, case: &str, request: &Value) -> f64 {
    let answered = emulator.ok("UpdateStateMachineAlias", request).await;
    answered
        .get("updateDate")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected an updateDate in {answered:?}"))
}

/// `(status, code, message)` of the alias READ once the delete has taken the record
/// away — looked for again until the read is refused, which is the CONDITION this
/// waits for, on a deadline that only bounds a delete that never lands.
async fn refused_read(emulator: &Emulator, case: &str, alias_arn: &str) -> (u16, String, String) {
    let request = json!({ "stateMachineAliasArn": alias_arn });
    let began = Instant::now();
    loop {
        let (status, parsed) = emulator.call("DescribeStateMachineAlias", &request).await;
        if status != 200 {
            let message = parsed
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected a message in {parsed:?}"))
                .to_string();
            return (status, error_code(&parsed), message);
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the deleted alias {alias_arn} to stop reading back within \
             {PATIENCE:?}, got {parsed:?}"
        );
        ::tokio::time::sleep(BETWEEN_LOOKS).await;
    }
}

/// The alias ARNs `ListStateMachineAliases` reports for `machine`.
async fn listed_aliases(emulator: &Emulator, machine: &str) -> Vec<String> {
    let listed = emulator
        .ok(
            "ListStateMachineAliases",
            &json!({ "stateMachineArn": machine }),
        )
        .await;
    listed
        .get("stateMachineAliases")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected a stateMachineAliases array in {listed:?}"))
        .iter()
        .map(|entry| string(entry, "stateMachineAliasArn"))
        .collect()
}

/// How many times each refusal is asked for: it is the REPETITION that is asserted —
/// the same answer every time, because a refused mutation writes nothing.
const MUTATIONS_PER_ROW: usize = 3;

/// An `UpdateStateMachineAlias` naming an alias whose own record was deleted, while the
/// state machine behind it is still held, is refused in ONE fixed sentence that names
/// nothing of the request — where the alias READ path quotes the requested ARN, and a
/// repeat delete is served.
#[tokio::test]
async fn an_update_of_a_deleted_alias_states_the_condition_as_a_fixed_sentence() {
    let emulator = Emulator::new();

    for (index, row) in rows().iter().enumerate() {
        let Row {
            case,
            versions,
            alias,
            names,
        } = row;
        let machine =
            machine_with_versions(&emulator, &format!("alias-refusal-{index}"), *versions).await;
        let routing = routing_of(&machine, *versions);

        // The alias really existed: it is created, read back, and updated while it
        // lives, so what the refusals below report is a record that was there.
        let created = create_alias(&emulator, alias, Some("before the delete"), &routing).await;
        let alias_arn = string(&created, "stateMachineAliasArn");
        assert_eq!(
            alias_arn,
            format!("{machine}:{alias}"),
            "case {case:?}: expected the alias ARN to qualify its state machine, got {created:?}"
        );
        let described = emulator
            .ok(
                "DescribeStateMachineAlias",
                &json!({ "stateMachineAliasArn": &alias_arn }),
            )
            .await;
        assert_eq!(
            string(&described, "name"),
            *alias,
            "case {case:?}: expected the created alias to read back, got {described:?}"
        );
        let request = update_request(&alias_arn, *names, &routing);
        let before = updated(&emulator, case, &request).await;
        assert!(
            before > 0.0,
            "case {case:?}: expected the live update to report an updateDate, got {before}"
        );

        // A second alias of the SAME state machine, which is never deleted: whatever
        // the refusals below answer, this one is still updatable afterwards.
        let survivor = string(
            &create_alias(&emulator, "SURVIVOR", None, &routing).await,
            "stateMachineAliasArn",
        );

        emulator
            .ok(
                "DeleteStateMachineAlias",
                &json!({ "stateMachineAliasArn": &alias_arn }),
            )
            .await;

        // The control: the alias's own reader quotes the ARN the request supplied.
        let (read_status, read_code, read_message) =
            refused_read(&emulator, case, &alias_arn).await;
        assert_eq!(
            (read_status, read_code.as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFound"),
            "case {case:?}: expected the read of the deleted alias to be refused \
             {CLIENT_ERROR_STATUS} ResourceNotFound, got {read_status} {read_code} with message \
             {read_message:?}"
        );
        assert_eq!(
            read_message,
            format!("Resource not found: '{alias_arn}'"),
            "case {case:?}: expected the read path to quote the requested ARN, got \
             {read_message:?}"
        );

        // The rule: the mutation states the condition as a sentence of its own, the
        // same answer however often it is asked.
        for attempt in 0..MUTATIONS_PER_ROW {
            let (status, code, message) = failure(
                &emulator,
                case,
                "UpdateStateMachineAlias",
                &request,
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFound"),
                "case {case:?}, attempt {attempt}: expected {CLIENT_ERROR_STATUS} \
                 ResourceNotFound, got {status} {code} with message {message:?}"
            );
            assert_eq!(
                message, ALIAS_GONE_SENTENCE,
                "case {case:?}, attempt {attempt}: expected the message \
                 {ALIAS_GONE_SENTENCE:?}, got {message:?}"
            );
            // Nothing of the request is interpolated into it — neither the ARN, the
            // alias name nor the state machine — which is what makes the two alias
            // wordings distinguishable.
            for (part, value) in [
                ("the alias ARN", alias_arn.as_str()),
                ("the alias name", *alias),
                ("the state machine ARN", machine.as_str()),
            ] {
                assert!(
                    !message.contains(value),
                    "case {case:?}, attempt {attempt}: expected the sentence to carry no value \
                     of the request, got {part} {value:?} inside {message:?}"
                );
            }
            assert_ne!(
                message, read_message,
                "case {case:?}, attempt {attempt}: the mutation and the read word this refusal \
                 differently, got {message:?} for both"
            );
        }

        // The refused ARN is reported in the shape's own member, which is where a
        // caller reads it instead of out of the sentence.
        let (_, refused) = emulator.call("UpdateStateMachineAlias", &request).await;
        assert_eq!(
            refused.get("resourceName").and_then(Value::as_str),
            Some(alias_arn.as_str()),
            "case {case:?}: expected the refused alias in resourceName, got {refused:?}"
        );

        // The boundary: the DELETE's post-condition already holds, so it is served
        // where the update is refused.
        emulator
            .ok(
                "DeleteStateMachineAlias",
                &json!({ "stateMachineAliasArn": &alias_arn }),
            )
            .await;

        // Nothing the refusals touched: the alias is gone from the listing, the
        // surviving alias of the same state machine still updates.
        let listed = listed_aliases(&emulator, &machine).await;
        assert!(
            !listed.contains(&alias_arn),
            "case {case:?}: expected the deleted alias to be gone from the listing, got {listed:?}"
        );
        assert!(
            listed.contains(&survivor),
            "case {case:?}: expected the surviving alias to still be listed, got {listed:?}"
        );
        let after = updated(
            &emulator,
            case,
            &update_request(&survivor, *names, &routing),
        )
        .await;
        assert!(
            after >= before,
            "case {case:?}: expected the surviving alias to still update, got {after} after \
             {before}"
        );
    }
}
