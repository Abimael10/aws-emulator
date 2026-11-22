// SPDX-License-Identifier: CC0-1.0

//! What a definition may write in a field whose values the Amazon States Language NAMES,
//! judged where the definition is SUBMITTED.
//!
//! Driven end to end over this service's own wire protocol through the production router,
//! so what is asserted is what a signed SDK request observes — the refusal
//! `CreateStateMachine` answers and the `diagnostics[]`
//! `ValidateStateMachineDefinition` reports for the same definition, which cannot
//! disagree because one walk answers both.
//!
//! The rule under test: a field of a definition whose value the language restricts to a
//! NAMED SET is refused `InvalidDefinition` where the definition writes a value outside
//! that set, stating the values the field admits at the field itself — so an authoring
//! error of this kind is caught by a client's definition gate rather than surfacing at run
//! time, if at all. A value of any other JSON type is outside the set like any unknown
//! name is, and the names are compared exactly, so a value differing from an admitted one
//! only in case is outside it too.
//!
//! The rows span what the rule generalises over: WHICH field (each of the catalogued
//! ones, which is more than the `CSVDelimiter` that exposed the defect), the OBJECT the
//! field sits in (an `ItemReader`'s `ReaderConfig`, a processor's `ProcessorConfig`, a
//! `ResultWriter`'s `WriterConfig`, one retrier of a `Retry` array), and the FORM of the
//! value written (an unknown name, a name differing only in case, and a value of another
//! JSON type). The complement rows are every value each set admits, each of which is
//! created — so the rule is about the set and not about the field.
//!
//! The sets are the language's own, written out here so that this suite pins them rather
//! than reading the same table the service judges by; each is cited on its entry.
//!
//! The harness (`Emulator`, `error_code`, `string`, `ROLE_ARN`) is the one
//! `state_machine_lifecycle` builds and the validation readers are
//! `definition_validation`'s; `tests/all/main.rs` picks this file up, so each is reached
//! through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    INVALID_STATE_MACHINE_DEFINITION, VALIDATION_RESULT_FAIL, VALIDATION_RESULT_OK,
};

use crate::definition_validation::{diagnostics, result_of, validate};
use crate::state_machine_lifecycle::{error_code, string, Emulator, ROLE_ARN};

/// The code every defect of the language's own schema is reported under.
const SCHEMA_VALIDATION_FAILED: &str = "SCHEMA_VALIDATION_FAILED";

/// What `CreateStateMachine` answers for `definition`: the status, the error code, and
/// the message.
async fn created(emulator: &Emulator, name: &str, definition: &Value) -> (u16, String, String) {
    let body = json!({
        "name": name,
        "definition": definition.to_string(),
        "roleArn": ROLE_ARN,
    });
    let (status, answered) = emulator.call("CreateStateMachine", &body).await;
    let message = match status {
        200 => String::new(),
        _ => string(&answered, "message"),
    };
    (status, error_code(&answered), message)
}

/// The refusal message `CreateStateMachine` states for a definition carrying the one
/// problem `subject` states: the service's own envelope over that subject.
fn refusal(subject: &str) -> String {
    format!("{INVALID_STATE_MACHINE_DEFINITION}: '{subject}'")
}

/// One subject of such a refusal: the code, the detail and the place it was found.
fn subject(detail: &str, location: &str) -> String {
    format!("{SCHEMA_VALIDATION_FAILED}: {detail} at {location}")
}

/// The sentence a diagnostic states for a value outside the set its field admits.
fn admitted(values: &[&str]) -> String {
    format!("Value should be one of the following: [{}]", values.join(", "))
}

/// A `Map` state whose `ItemReader` reads one S3 object under `config`.
fn reader(config: Value) -> Value {
    map_state(json!({
        "ItemReader": {
            "Resource": "arn:aws:states:::s3:getObject",
            "ReaderConfig": config,
            "Parameters": { "Bucket": "a-bucket", "Key": "a-key" },
        },
    }))
}

/// A `Map` state whose processor runs under `config`.
fn processor(config: Value) -> Value {
    map_state(json!({ "ProcessorConfig": config }))
}

/// A `Map` state whose `ResultWriter` writes under `config`.
fn writer(config: Value) -> Value {
    map_state(json!({
        "ResultWriter": {
            "Resource": "arn:aws:states:::s3:putObject",
            "WriterConfig": config,
            "Parameters": { "Bucket": "a-bucket" },
        },
    }))
}

/// A one-state `Map` definition carrying `members` beside its processor; a
/// `ProcessorConfig` written in `members` is moved into that processor, which is the one
/// position the language declares it at.
fn map_state(members: Value) -> Value {
    let mut state = json!({
        "Type": "Map",
        "ItemProcessor": {
            "StartAt": "P",
            "States": { "P": { "Type": "Pass", "End": true } },
        },
        "End": true,
    });
    for (member, value) in members.as_object().expect("the members are an object") {
        if member == "ProcessorConfig" {
            state["ItemProcessor"]["ProcessorConfig"] = value.clone();
            continue;
        }
        state[member] = value.clone();
    }
    json!({ "StartAt": "M", "States": { "M": state } })
}

/// A one-state `Task` definition whose `Retry` carries one retrier of the shape
/// `retrier`.
fn retrier(retrier: Value) -> Value {
    let mut entry = json!({ "ErrorEquals": ["States.ALL"] });
    for (member, value) in retrier.as_object().expect("the retrier is an object") {
        entry[member] = value.clone();
    }
    json!({
        "StartAt": "T",
        "States": {
            "T": {
                "Type": "Task",
                "Resource": "arn:aws:states:::aws-sdk:sts:getCallerIdentity",
                "Retry": [entry],
                "End": true,
            },
        },
    })
}

/// A field whose value the language names is refused where the definition writes a value
/// outside the set, stating the values it admits at the field itself.
#[tokio::test]
async fn a_value_outside_the_set_its_field_admits_is_refused_at_the_field() {
    // (case, the definition, the location it is reported at, the values the field admits)
    let rows: Vec<(&str, Value, &str, Vec<&str>)> = vec![
        (
            // "CSVDelimiter ... Valid values: COMMA | PIPE | SEMICOLON | SPACE | TAB"
            // (the `ItemReader` reference of the Amazon States Language; the ORDER is
            // the service's own, as the refusal states it).
            "a CSV delimiter no name of the set stands for",
            reader(json!({ "InputType": "CSV", "CSVDelimiter": "COLON" })),
            "/States/M/ItemReader/ReaderConfig/CSVDelimiter",
            vec!["COMMA", "SEMICOLON", "PIPE", "SPACE", "TAB"],
        ),
        (
            "the same field written as a value of another JSON type",
            reader(json!({ "InputType": "CSV", "CSVDelimiter": 44 })),
            "/States/M/ItemReader/ReaderConfig/CSVDelimiter",
            vec!["COMMA", "SEMICOLON", "PIPE", "SPACE", "TAB"],
        ),
        (
            "the same field written in another case",
            reader(json!({ "InputType": "CSV", "CSVDelimiter": "comma" })),
            "/States/M/ItemReader/ReaderConfig/CSVDelimiter",
            vec!["COMMA", "SEMICOLON", "PIPE", "SPACE", "TAB"],
        ),
        (
            // "InputType ... Valid values: CSV | JSON | JSONL | MANIFEST".
            "a dataset format the language does not name",
            reader(json!({ "InputType": "PARQUET" })),
            "/States/M/ItemReader/ReaderConfig/InputType",
            vec!["CSV", "JSON", "JSONL", "MANIFEST"],
        ),
        (
            // "CSVHeaderLocation ... Valid values: FIRST_ROW | GIVEN".
            "a header location neither name of the pair stands for",
            reader(json!({ "InputType": "CSV", "CSVHeaderLocation": "SECOND_ROW" })),
            "/States/M/ItemReader/ReaderConfig/CSVHeaderLocation",
            vec!["FIRST_ROW", "GIVEN"],
        ),
        (
            // "Mode ... Valid values: INLINE | DISTRIBUTED" (the `ProcessorConfig`
            // reference).
            "a processor mode the language does not name",
            processor(json!({ "Mode": "DISTRIBUTED_MAP" })),
            "/States/M/ItemProcessor/ProcessorConfig/Mode",
            vec!["INLINE", "DISTRIBUTED"],
        ),
        (
            // "ExecutionType ... Valid values: STANDARD | EXPRESS", the two workflow
            // types a child workflow execution runs as.
            "a child execution type the language does not name",
            processor(json!({ "Mode": "DISTRIBUTED", "ExecutionType": "SYNC" })),
            "/States/M/ItemProcessor/ProcessorConfig/ExecutionType",
            vec!["STANDARD", "EXPRESS"],
        ),
        (
            // "OutputType ... Valid values: JSON | JSONL" (the `ResultWriter`
            // reference).
            "a result format the language does not name",
            writer(json!({ "OutputType": "PARQUET" })),
            "/States/M/ResultWriter/WriterConfig/OutputType",
            vec!["JSON", "JSONL"],
        ),
        (
            // "Transformation ... Valid values: NONE | COMPACT | FLATTEN".
            "a result transformation the language does not name",
            writer(json!({ "OutputType": "JSON", "Transformation": "FLATTENED" })),
            "/States/M/ResultWriter/WriterConfig/Transformation",
            vec!["NONE", "COMPACT", "FLATTEN"],
        ),
        (
            // "JitterStrategy ... If you choose FULL ... If you choose NONE", the two
            // strategies a retrier spreads its waits by.
            "a jitter strategy neither name of the pair stands for",
            retrier(json!({ "JitterStrategy": "HALF" })),
            "/States/T/Retry[0]/JitterStrategy",
            vec!["FULL", "NONE"],
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition, location, values)) in rows.iter().enumerate() {
        let expected = refusal(&subject(&admitted(values), location));
        let (status, code, message) =
            created(&emulator, &format!("named-value-{index}"), definition).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidDefinition"),
            "case {case:?}: expected 400 InvalidDefinition, got {status} {code} with {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        // The validation API reports the same one diagnostic, at the same place: one
        // walk answers both.
        let validated = validate(
            &emulator,
            case,
            &json!({ "definition": definition.to_string() }),
        )
        .await;
        assert_eq!(
            result_of(&validated),
            VALIDATION_RESULT_FAIL,
            "case {case:?}: expected a failing validation, got {validated:?}"
        );
        let located: Vec<(String, String)> = diagnostics(&validated)
            .iter()
            .map(|diagnostic| (string(diagnostic, "location"), string(diagnostic, "message")))
            .collect();
        assert_eq!(
            located,
            vec![((*location).to_string(), admitted(values))],
            "case {case:?}: expected one diagnostic at the field, got {validated:?}"
        );
    }
}

/// One set of values the language names: the field it is written at, the values it
/// admits, and the definition that writes one of them at that field.
struct NamedSet {
    field: &'static str,
    values: Vec<&'static str>,
    definition: fn(&str) -> Value,
}

/// The complement: EVERY value each of those sets admits is created, so the rule is about
/// the set rather than about the field.
#[tokio::test]
async fn every_value_a_set_admits_is_created() {
    let sets: Vec<NamedSet> = vec![
        NamedSet {
            field: "CSVDelimiter",
            values: vec!["COMMA", "SEMICOLON", "PIPE", "SPACE", "TAB"],
            definition: |named| reader(json!({ "InputType": "CSV", "CSVDelimiter": named })),
        },
        NamedSet {
            field: "InputType",
            values: vec!["CSV", "JSON", "JSONL", "MANIFEST"],
            definition: |named| reader(json!({ "InputType": named })),
        },
        NamedSet {
            field: "CSVHeaderLocation",
            values: vec!["FIRST_ROW", "GIVEN"],
            definition: |named| {
                reader(json!({
                    "InputType": "CSV",
                    "CSVHeaderLocation": named,
                    "CSVHeaders": ["a", "b"],
                }))
            },
        },
        NamedSet {
            field: "Mode",
            values: vec!["INLINE", "DISTRIBUTED"],
            definition: |named| processor(json!({ "Mode": named })),
        },
        NamedSet {
            field: "ExecutionType",
            values: vec!["STANDARD", "EXPRESS"],
            definition: |named| processor(json!({ "Mode": "DISTRIBUTED", "ExecutionType": named })),
        },
        NamedSet {
            field: "OutputType",
            values: vec!["JSON", "JSONL"],
            definition: |named| writer(json!({ "OutputType": named })),
        },
        NamedSet {
            field: "Transformation",
            values: vec!["NONE", "COMPACT", "FLATTEN"],
            definition: |named| writer(json!({ "OutputType": "JSON", "Transformation": named })),
        },
        NamedSet {
            field: "JitterStrategy",
            values: vec!["FULL", "NONE"],
            definition: |named| retrier(json!({ "JitterStrategy": named })),
        },
    ];

    let emulator = Emulator::new();
    let mut index = 0usize;
    for set in sets {
        for named in set.values {
            let case = format!("{} = {named}", set.field);
            let written = (set.definition)(named);
            let (status, code, message) =
                created(&emulator, &format!("admitted-{index}"), &written).await;
            index += 1;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the definition to be created, got {status} {code} with \
                 {message:?}"
            );
            let validated = validate(
                &emulator,
                &case,
                &json!({ "definition": written.to_string() }),
            )
            .await;
            assert_eq!(
                result_of(&validated),
                VALIDATION_RESULT_OK,
                "case {case:?}: expected a passing validation, got {validated:?}"
            );
        }
    }
}
