// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the ONE rule
//! `ValidateTelemetryPipelineConfiguration` answers an undefined property of a
//! configuration document by, wherever in the document that property is written.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`,
//! restJson1 serialization, the modelled-constraint walk and the handler — through the
//! ONE harness this test crate has (`telemetry_rules`), so what is asserted is what a
//! signed AWS SDK request observes.
//!
//! The rule: a property a configuration document writes that the pipeline schema does
//! not define — at the document's root, in a section, the processor a POSITION of a
//! `processor` sequence names, or an option that position configures that processor with
//! — is reported in the 200 `Errors` list as the schema's unknown-property defect: the
//! `UNKNOWN_PROPERTY` reason, the wording a schema validator reports an undefined
//! property with, naming that property exactly as the body writes it, and a `FieldMap`
//! pinning the path the property is written at. Which processors the schema defines
//! depends on the section's own source; which processors THIS emulator can run does not
//! enter the report at all, so neither a reason nor a message describing the emulator's
//! own capabilities is ever answered.

use serde_json::{json, Value};

use crate::telemetry_rules::{call, emulator, Emulator};

/// The region every request below signs for.
const HOME_REGION: &str = "us-west-2";

/// The category a schema validator reports a property it does not define under, the
/// category this service reports a document that declares something a pipeline
/// definition may not under, and the `FieldMap` key a defect pinned to a place in the
/// document carries.
const UNKNOWN_PROPERTY: &str = "UNKNOWN_PROPERTY";
const VALIDATION_ERROR: &str = "PIPELINE_CONFIGURATION_VALIDATION_ERROR";
const PATH_KEY: &str = "path";

/// The sentence a schema validator reports an undefined property with: the wording this
/// service answers for every property the pipeline schema does not define, whichever
/// place of the document writes it.
fn undefined(property: &str) -> String {
    format!(
        "property '{property}' is not defined in the schema and the schema does not allow \
         additional properties"
    )
}

/// One `ValidateTelemetryPipelineConfiguration` call, which answers 200 whatever the
/// document's defects are: they are REPORTED in the output's list, not refused.
async fn validate(case: &str, emulator: &Emulator, configuration: &str) -> Value {
    let (status, response) = call(
        emulator,
        HOME_REGION,
        "/ValidateTelemetryPipelineConfiguration",
        &json!({ "Configuration": { "Body": configuration } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a defect is reported, not refused; expected 200, got {status} \
         {response:?}"
    );
    response
}

/// Every reported defect as a `(Reason, Message, the FieldMap's path)` triple, sorted so
/// a case asserts the SET of defects rather than the order two independent checks ran
/// in. A defect that is about no single place of the document carries no `path`, which
/// shows as an empty one.
fn reported(case: &str, response: &Value) -> Vec<(String, String, String)> {
    let mut reported: Vec<(String, String, String)> = response
        .get("Errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|error| {
            let reason = error
                .get("Reason")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every defect carries a Reason, got {error:?}")
                })
                .to_string();
            let message = error
                .get("Message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every defect says what is wrong, got {error:?}")
                })
                .to_string();
            let path = error
                .pointer(&format!("/FieldMap/{PATH_KEY}"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (reason, message, path)
        })
        .collect();
    reported.sort();
    reported
}

/// Only the defects of `reason`, which is how a case asserts its own class of defect
/// without restating the other classes the same document happens to be reported for.
fn reported_as(case: &str, response: &Value, reason: &str) -> Vec<(String, String, String)> {
    let mut found: Vec<(String, String, String)> = reported(case, response)
        .into_iter()
        .filter(|(found, _, _)| found == reason)
        .collect();
    found.sort();
    found
}

/// One expected undefined-property defect: the property the document writes and the path
/// it writes it at.
fn defect(property: &str, path: &str) -> (String, String, String) {
    (
        UNKNOWN_PROPERTY.to_string(),
        undefined(property),
        path.to_string(),
    )
}

/// A pipeline definition written at the top level, with the `source` and `processor`
/// text given and one sink.
fn top_level(source: &str, processor: &str) -> String {
    format!(
        "{source}\
         {processor}\
         sink:\n  \
           - cloudwatch_logs:\n      \
               log_group: /aws/telemetry/example\n"
    )
}

/// A third-party source, which pins none of the schema's vended-log branches.
const THIRD_PARTY_SOURCE: &str = "source:\n  http: {}\n";

/// A source reading log events AWS itself vends, whose branches name their own
/// processors.
const VENDED_SOURCE: &str =
    "source:\n  cloudwatch_logs:\n    data_sources:\n      - aws_cloudtrail\n";

// =============================================================================
// A property the schema does not define, wherever it is written
// =============================================================================

/// Every property the pipeline schema does not define is reported as the undefined
/// property it is, at the path the document writes it at.
///
/// The rows differ along what the rule generalises over: WHICH KIND of property is
/// undefined (a property of the document's root, a property of a section, the processor
/// a position names, an option a position configures), where it is written (the
/// document's root, a top-level pipeline, a named section, each of several positions of
/// one sequence, one position of each of two chained sections), how many undefined
/// properties one document has, which source the section reads from (a third-party one,
/// an AWS-vended one, none at all), and how the document is spelled (YAML, and the JSON
/// it is a superset of).
///
/// The processor names and option names each row writes are read against the processor
/// catalogue this format's chain compiles against (`PROCESSORS` in
/// `src/ops/test_telemetry_pipeline.rs`) and the vended-log branch table beside the
/// schema walk (`RESTRICTIONS` in
/// `src/ops/validate_telemetry_pipeline_configuration/vended_log_schema.rs`).
#[tokio::test]
async fn a_property_the_schema_does_not_define_is_reported_at_its_path() {
    let emulator = emulator();

    for (case, configuration, expected) in [
        (
            "the processor a position of a third-party section names",
            top_level(THIRD_PARTY_SOURCE, "processor:\n  - grok: {}\n"),
            vec![defect("grok", "processor[0]")],
        ),
        (
            "a processor named with the options it would take",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - date:\n      from_time_received: true\n",
            ),
            vec![defect("date", "processor[0]")],
        ),
        (
            "two positions of one sequence, each reported on its own",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - parse_json: {}\n  - geoip: {}\n  - mutate_event: {}\n",
            ),
            vec![
                defect("geoip", "processor[1]"),
                defect("mutate_event", "processor[2]"),
            ],
        ),
        (
            "a position of a NAMED section, named under that section",
            "log-pipeline:\n  \
               source:\n    \
                 http: {}\n  \
               processor:\n    \
                 - grok: {}\n  \
               sink:\n    \
                 - cloudwatch_logs:\n        \
                     log_group: /aws/x\n"
                .to_string(),
            vec![defect("grok", "log-pipeline.processor[0]")],
        ),
        (
            "one position in each of two chained sections",
            "entry:\n  \
               source:\n    \
                 http: {}\n  \
               processor:\n    \
                 - grok: {}\n  \
               sink:\n    \
                 - pipeline:\n        \
                     name: exit\n\
             exit:\n  \
               source:\n    \
                 pipeline:\n      \
                   name: entry\n  \
               processor:\n    \
                 - geoip: {}\n  \
               sink:\n    \
                 - cloudwatch_logs:\n        \
                     log_group: /aws/x\n"
                .to_string(),
            vec![
                defect("grok", "entry.processor[0]"),
                defect("geoip", "exit.processor[0]"),
            ],
        ),
        (
            "the same document spelled as the JSON YAML is a superset of",
            json!({
                "log-pipeline": {
                    "source": { "http": {} },
                    "processor": [{ "grok": {} }],
                    "sink": [{ "cloudwatch_logs": { "log_group": "/aws/x" } }],
                }
            })
            .to_string(),
            vec![defect("grok", "log-pipeline.processor[0]")],
        ),
        (
            "an option the schema does not define for the processor it configures",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - csv:\n      delete_source: true\n",
            ),
            vec![defect("delete_source", "processor[0].csv.delete_source")],
        ),
        (
            "two undefined options of one position",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - parse_json:\n      source: message\n      \
                   handle_failed_events: skip\n      tags_on_failure: [\"bad\"]\n",
            ),
            vec![
                defect(
                    "handle_failed_events",
                    "processor[0].parse_json.handle_failed_events",
                ),
                defect("tags_on_failure", "processor[0].parse_json.tags_on_failure"),
            ],
        ),
        (
            "an undefined processor AND an undefined section property",
            "log-pipeline:\n  source:\n    http: {}\n  retries: 3\n  processor:\n    \
             - grok: {}\n  sink:\n    - cloudwatch_logs: {}\n"
                .to_string(),
            vec![
                defect("grok", "log-pipeline.processor[0]"),
                defect("retries", "log-pipeline.retries"),
            ],
        ),
        (
            "an undefined processor AND a property of the document's own root",
            "owner: team\nsource:\n  http: {}\nprocessor:\n  - grok: {}\nsink:\n  \
             - cloudwatch_logs: {}\n"
                .to_string(),
            vec![defect("grok", "processor[0]"), defect("owner", "root")],
        ),
        (
            "a processor a vended-log branch does not define, read from a vended source",
            top_level(VENDED_SOURCE, "processor:\n  - grok: {}\n"),
            vec![defect("grok", "processor[0]")],
        ),
        (
            "a processor the general vocabulary defines, which a vended branch does not",
            top_level(VENDED_SOURCE, "processor:\n  - lowercase_string:\n      \
               with_keys: [\"message\"]\n"),
            vec![defect("lowercase_string", "processor[0]")],
        ),
        (
            "a position of a section that declares no source at all, which pins no branch",
            "pipeline:\n  processor:\n    - geoip: {}\n  sink:\n    \
             - cloudwatch_logs: {}\n"
                .to_string(),
            vec![defect("geoip", "processor[0]")],
        ),
    ] {
        let response = validate(case, &emulator, &configuration).await;
        let mut expected = expected;
        expected.sort();
        assert_eq!(
            reported_as(case, &response, UNKNOWN_PROPERTY),
            expected,
            "case {case:?}: expected those undefined-property defects for {configuration:?}, \
             got {response:?}"
        );
    }
}

/// An IAM role ARN of the form this service's model declares
/// (`smithy_query(query="shape", shape="IamRoleArn")`), so the role a source assumes is
/// no defect of the rows below.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/TelemetryPipelineRole";

/// A section whose source names a data source no vended-log branch is about pins no
/// branch, so the ONE property such a document writes that the schema does not define is
/// the only defect reported for it: one entry, naming that property and the position it
/// is written at, and nothing else.
///
/// The rows differ along what the rule generalises over: which processor name is
/// undefined, which position of the sequence writes it (the only one, the second of
/// two), what the source names its own data source, and whether the source assumes a
/// role.
#[tokio::test]
async fn a_document_whose_one_undefined_property_is_a_processor_reports_that_one_defect() {
    let emulator = emulator();

    for (case, configuration, expected) in [
        (
            "the only position of the sequence",
            top_level(
                &format!(
                    "source:\n  cloudwatch_logs:\n    log_event_metadata:\n      \
                       data_source_name: \"probe_feed\"\n      \
                       data_source_type: \"default\"\n    aws:\n      \
                       sts_role_arn: \"{ROLE_ARN}\"\n"
                ),
                "processor:\n  - no_such_processor: {}\n",
            ),
            defect("no_such_processor", "processor[0]"),
        ),
        (
            "the second position, after one the schema defines",
            top_level(
                "source:\n  cloudwatch_logs:\n    log_event_metadata:\n      \
                   data_source_name: \"other_feed\"\n",
                "processor:\n  - parse_json: {}\n  - translate_entries: {}\n",
            ),
            defect("translate_entries", "processor[1]"),
        ),
    ] {
        let response = validate(case, &emulator, &configuration).await;
        assert_eq!(
            reported(case, &response),
            vec![expected],
            "case {case:?}: expected that one defect for {configuration:?}, got {response:?}"
        );
    }
}

/// A position of a `processor` sequence that does not name ONE processor is a defect of
/// the sequence's own FORM, not an undefined property: the schema cannot say which
/// processor such a position means, so it is reported as the form it is written in and no
/// undefined-property defect is answered for it.
///
/// The rows span the forms a position can take that name no single processor: a bare
/// name rather than a mapping, two processors in one position, and a position carrying
/// nothing at all.
#[tokio::test]
async fn a_position_that_names_no_single_processor_is_a_form_defect() {
    let emulator = emulator();

    for (case, configuration) in [
        (
            "a position written as a bare name rather than a mapping",
            top_level(THIRD_PARTY_SOURCE, "processor:\n  - grok\n"),
        ),
        (
            "a position naming two processors at once",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - csv: {}\n    trim_string: {}\n",
            ),
        ),
        (
            "a position carrying nothing at all",
            top_level(THIRD_PARTY_SOURCE, "processor:\n  -\n"),
        ),
    ] {
        let response = validate(case, &emulator, &configuration).await;
        let reported = reported(case, &response);
        assert_eq!(
            reported_as(case, &response, UNKNOWN_PROPERTY),
            Vec::new(),
            "case {case:?}: a position naming no single processor has no property to report \
             undefined, got {response:?}"
        );
        assert!(
            reported
                .iter()
                .any(|(reason, _, _)| reason == VALIDATION_ERROR),
            "case {case:?}: expected the sequence's form to be reported for \
             {configuration:?}, got {reported:?}"
        );
    }
}

// =============================================================================
// A property the schema DOES define is no defect, and nothing reports a capability
// =============================================================================

/// A document that writes only properties the schema defines is reported no
/// undefined-property defect — including a position naming one of the vended-log
/// parsers, which the schema defines for the source that pins its branch whether or not
/// this emulator runs that parser itself.
///
/// The rows differ along what the rule generalises over: which vocabulary names the
/// processor (the general one, a vended-log branch's per-source parser, the parser a
/// branch shares), how many positions the sequence has, which source the section reads
/// from, and whether the position carries options the schema defines or none at all.
#[tokio::test]
async fn a_property_the_schema_defines_is_no_defect() {
    let emulator = emulator();

    for (case, configuration) in [
        (
            "a general processor with the options the schema defines for it",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - csv:\n      delimiter: \",\"\n      \
                   column_names: [\"a\", \"b\"]\n",
            ),
        ),
        (
            "several general processors, each carrying its own options",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - parse_json:\n      source: message\n  \
                   - trim_string:\n      with_keys: [\"message\"]\n  \
                   - uppercase_string:\n      with_keys: [\"message\"]\n",
            ),
        ),
        (
            "the parser a vended source's own branch admits, which this emulator does \
             not itself run",
            top_level(VENDED_SOURCE, "processor:\n  - ocsf: {}\n"),
        ),
        (
            "another vended source's per-source parser",
            top_level(
                "source:\n  amazon_vpc_flow: {}\n",
                "processor:\n  - parse_vpc: {}\n",
            ),
        ),
        (
            "a section that transforms nothing at all",
            top_level(THIRD_PARTY_SOURCE, ""),
        ),
    ] {
        let response = validate(case, &emulator, &configuration).await;
        assert_eq!(
            reported_as(case, &response, UNKNOWN_PROPERTY),
            Vec::new(),
            "case {case:?}: expected no undefined-property defect for {configuration:?}, \
             got {response:?}"
        );
        assert_eq!(
            reported(case, &response),
            Vec::new(),
            "case {case:?}: a configuration the schema defines every property of is \
             reported no defect at all, got {response:?}"
        );
    }
}

/// Nothing a report answers is about what THIS emulator can run: no reason code and no
/// message of any defect names the emulator or the processors it happens to implement,
/// whichever class of defect the document has.
///
/// The rows span the classes a processor position is reported under: a position naming
/// a processor no vocabulary defines, a position configuring a processor with an
/// undefined option, a position whose option carries a value of the wrong form, and a
/// section reading a vended source with a processor its branch does not admit.
#[tokio::test]
async fn no_reported_defect_describes_this_emulators_own_capabilities() {
    let emulator = emulator();

    for (case, configuration) in [
        (
            "a processor no vocabulary defines",
            top_level(THIRD_PARTY_SOURCE, "processor:\n  - grok: {}\n"),
        ),
        (
            "an option no processor defines",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - csv:\n      delete_source: true\n",
            ),
        ),
        (
            "an option carrying a value of the wrong form",
            top_level(
                THIRD_PARTY_SOURCE,
                "processor:\n  - csv:\n      delimiter: \"not one character\"\n",
            ),
        ),
        (
            "a vended source running a processor its branch does not admit",
            top_level(VENDED_SOURCE, "processor:\n  - parse_json: {}\n"),
        ),
    ] {
        let response = validate(case, &emulator, &configuration).await;
        let reported = reported(case, &response);
        assert!(
            !reported.is_empty(),
            "case {case:?}: expected the document to be reported a defect for \
             {configuration:?}, got {response:?}"
        );
        for (reason, message, path) in &reported {
            for leaked in ["emulator", "does not run"] {
                assert!(
                    !message.contains(leaked) && !reason.contains(leaked),
                    "case {case:?}: no defect describes what this emulator runs, got \
                     {reason} {message:?} at {path:?}"
                );
            }
            assert_ne!(
                reason, "UNSUPPORTED_PROCESSOR",
                "case {case:?}: a configuration's defects are reported in the schema's own \
                 categories, got {message:?} at {path:?}"
            );
        }
    }
}
