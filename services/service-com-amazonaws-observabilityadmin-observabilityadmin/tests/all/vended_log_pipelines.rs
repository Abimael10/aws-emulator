// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the pipelines that read log events AWS itself VENDS: what
//! `ValidateTelemetryPipelineConfiguration` reports about their processors, and how a
//! read names their data source.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`,
//! restJson1 serialization, the modelled-constraint walk and the handlers — through the
//! ONE harness this test crate has (`telemetry_rules`), so what is asserted is what a
//! signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! 1. a pipeline section reading from an AWS-vended log source runs a RESTRICTED
//!    processor set — one branch of the schema per source type, and per vended data
//!    source for a source type whose restriction is per data source — so a section that
//!    violates a branch is reported one defect for that branch, under the restriction's
//!    own category and keyed by the restriction's descriptors; a section that declares no
//!    source pins no branch, so every branch it violates is reported, and a section
//!    reading from a third-party source carries none of these restrictions;
//! 2. a POSITION of such a section's processor sequence naming a processor that branch of
//!    the schema does not define at all is reported as the undefined property it is, at
//!    that position's path;
//! 3. a read of a pipeline names each of its data sources the way `DataSource$Name` says
//!    that KIND of source names one: a source reading AWS-vended log events reports the
//!    `data_source_name` the metadata of the events vended FOR THAT PIPELINE carries —
//!    the pipeline's own name as an IDENTIFIER, which separates its words with `_` where
//!    the resource name separates them with `-`, whatever name the configuration document
//!    declares for that source — while a third-party source reports the configured name,
//!    or the plugin's own name when the document configures none.

use serde_json::{json, Value};

use crate::telemetry_rules::{call, emulator, Emulator};

/// The region every request below signs for.
const HOME_REGION: &str = "us-west-2";

/// The category a defect of one of the schema's vended-log restrictions carries, and the
/// category a position naming a processor the schema does not define carries.
const RESTRICTED: &str = "DATA_SOURCE_PROCESSOR_RESTRICTED";
const UNKNOWN_PROPERTY: &str = "UNKNOWN_PROPERTY";

/// The `FieldMap` key a defect pinned to a place in the document carries.
const PATH_KEY: &str = "path";

/// One `ValidateTelemetryPipelineConfiguration` call.
async fn validate(emulator: &Emulator, configuration: &str) -> (u16, Value) {
    let (status, response) = call(
        emulator,
        HOME_REGION,
        "/ValidateTelemetryPipelineConfiguration",
        &json!({ "Configuration": { "Body": configuration } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a configuration's defects are REPORTED, so the call answers 200, got {status} \
         {response:?}"
    );
    (status, response)
}

/// Every reported defect of `reason`, as `(Message, the FieldMap's key/value pairs)`
/// sorted — so a case asserts the SET of defects rather than the order the schema's
/// branches were walked in.
fn reported(case: &str, response: &Value, reason: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut found: Vec<(String, Vec<(String, String)>)> = response
        .get("Errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|error| error.get("Reason").and_then(Value::as_str) == Some(reason))
        .map(|error| {
            let message = error
                .get("Message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every defect says what is wrong, got {error:?}")
                })
                .to_string();
            let field_map = error
                .get("FieldMap")
                .and_then(Value::as_object)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every defect carries a FieldMap, got {error:?}")
                });
            let mut descriptors: Vec<(String, String)> = field_map
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            descriptors.sort();
            (message, descriptors)
        })
        .collect();
    found.sort();
    found
}

/// One expected defect, written the way [`reported`] reads one back.
fn defect(message: &str, field_map: &[(&str, &str)]) -> (String, Vec<(String, String)>) {
    let mut descriptors: Vec<(String, String)> = field_map
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    descriptors.sort();
    (message.to_string(), descriptors)
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

// =============================================================================
// Rules 1 and 2 — the vended-log branches of the schema
// =============================================================================

/// A section reading AWS-vended log events is reported one defect per branch of the
/// schema it violates, and one per processor position that branch does not define.
///
/// The rows differ along what the rule generalises over: which source the section
/// declares (none, each of the vended ones, a third-party one), which vended data
/// sources it names (one, none), how many processors it runs (none, one, two, three) and
/// whether the branch admits them, and whether the property a branch's source requires
/// is declared.
#[tokio::test]
async fn a_vended_log_section_is_reported_against_every_branch_it_violates() {
    let emulator = emulator();

    for (case, configuration, expected_restrictions, expected_positions) in [
        (
            "a section declaring no source, running two processors the vended branches \
             do not define",
            "pipeline:\n  processor:\n    - lowercase_string:\n        \
               with_keys: [\"message\"]\n    - uppercase_string:\n        \
               with_keys: [\"message\"]\n"
                .to_string(),
            vec![
                defect(
                    "must have at most 1 items but found 2",
                    &[
                        ("source_type", "cloudwatch_logs"),
                        ("data_source_name", "aws_cloudtrail"),
                        ("allowed_processor", "ocsf"),
                    ],
                ),
                defect(
                    "must have at most 1 items but found 2",
                    &[
                        ("source_type", "cloudwatch_logs"),
                        ("data_source_name", "aws_security_hub_cspm"),
                        ("allowed_processor", "ocsf"),
                    ],
                ),
                defect(
                    "property 'lowercase_string' is not defined in the schema and the \
                     schema does not allow additional properties",
                    &[
                        ("source_type", "amazon_vpc_flow"),
                        ("allowed_processors", "parse_vpc/ocsf"),
                    ],
                ),
                defect(
                    "property 'lowercase_string' is not defined in the schema and the \
                     schema does not allow additional properties",
                    &[
                        ("source_type", "amazon_route53"),
                        ("allowed_processors", "parse_route53/ocsf"),
                    ],
                ),
                defect(
                    "required property 'cloudfront' not found",
                    &[
                        ("source_type", "amazon_cloudfront"),
                        ("allowed_processors", "parse_cloudfront/ocsf"),
                    ],
                ),
            ],
            vec![
                defect(
                    "property 'lowercase_string' is not defined in the schema and the \
                     schema does not allow additional properties",
                    &[(PATH_KEY, "processor[0]")],
                ),
                defect(
                    "property 'uppercase_string' is not defined in the schema and the \
                     schema does not allow additional properties",
                    &[(PATH_KEY, "processor[1]")],
                ),
            ],
        ),
        (
            "a vended data source running a processor its own branch does not admit",
            top_level(
                "source:\n  cloudwatch_logs:\n    data_sources:\n      \
                   - aws_security_hub_cspm\n",
                "processor:\n  - parse_json: {}\n",
            ),
            vec![defect(
                "property 'parse_json' is not defined in the schema and the schema does \
                 not allow additional properties",
                &[
                    ("source_type", "cloudwatch_logs"),
                    ("data_source_name", "aws_security_hub_cspm"),
                    ("allowed_processor", "ocsf"),
                ],
            )],
            Vec::new(),
        ),
        (
            "a vended data source running exactly the processor its branch admits",
            top_level(
                "source:\n  cloudwatch_logs:\n    data_sources:\n      \
                   - aws_cloudtrail\n",
                "processor:\n  - ocsf: {}\n",
            ),
            Vec::new(),
            Vec::new(),
        ),
        (
            "a vended source running more processors than its branch admits",
            top_level(
                "source:\n  amazon_route53: {}\n",
                "processor:\n  - parse_route53: {}\n  - ocsf: {}\n  - parse_json: {}\n",
            ),
            vec![defect(
                "must have at most 2 items but found 3",
                &[
                    ("source_type", "amazon_route53"),
                    ("allowed_processors", "parse_route53/ocsf"),
                ],
            )],
            Vec::new(),
        ),
        (
            "a vended source declaring the property its branch requires, with a \
             processor that branch admits",
            top_level(
                "source:\n  amazon_cloudfront:\n    cloudfront:\n      \
                   distribution_id: E1EXAMPLE\n",
                "processor:\n  - parse_cloudfront: {}\n",
            ),
            Vec::new(),
            Vec::new(),
        ),
        (
            "a vended source leaving out the property its branch requires",
            top_level(
                "source:\n  amazon_cloudfront: {}\n",
                "processor:\n  - parse_cloudfront: {}\n",
            ),
            vec![defect(
                "required property 'cloudfront' not found",
                &[
                    ("source_type", "amazon_cloudfront"),
                    ("allowed_processors", "parse_cloudfront/ocsf"),
                ],
            )],
            Vec::new(),
        ),
        (
            "a vended source running the one processor its branch admits",
            top_level(
                "source:\n  amazon_vpc_flow: {}\n",
                "processor:\n  - parse_vpc: {}\n",
            ),
            Vec::new(),
            Vec::new(),
        ),
        (
            "a third-party source, which carries none of these restrictions",
            top_level(
                "source:\n  http: {}\n",
                "processor:\n  - uppercase_string:\n      with_keys: [\"message\"]\n",
            ),
            Vec::new(),
            Vec::new(),
        ),
        (
            "a section declaring no source and no processor, which restricts nothing",
            "pipeline:\n  sink:\n    - cloudwatch_logs: {}\n".to_string(),
            Vec::new(),
            Vec::new(),
        ),
    ] {
        let (_, response) = validate(&emulator, &configuration).await;
        let mut expected_restrictions = expected_restrictions;
        expected_restrictions.sort();
        assert_eq!(
            reported(case, &response, RESTRICTED),
            expected_restrictions,
            "case {case:?}: expected those restriction defects for {configuration:?}, \
             got {response:?}"
        );
        let mut expected_positions = expected_positions;
        expected_positions.sort();
        assert_eq!(
            reported(case, &response, UNKNOWN_PROPERTY),
            expected_positions,
            "case {case:?}: expected those undefined-processor defects for \
             {configuration:?}, got {response:?}"
        );
    }
}

// =============================================================================
// Rule 3 — how a read names a source's data sources, by the kind of source
// =============================================================================

/// How a read NAMES a pipeline's data sources is `DataSource$Name`'s split by the KIND of
/// source — `smithy_query(query="doc", shape="DataSource", member="Name")`: "For
/// CloudWatch Logs sources, this corresponds to the `data_source_name` from the log event
/// metadata. For third-party sources, this is either the configured `data_source_name` or
/// defaults to the plugin name if not specified."
///
/// So a source reading AWS-vended log events reports the `data_source_name` the metadata
/// carries, and those events are the ones vended for the pipeline ITSELF: the metadata
/// names it by the pipeline's own IDENTIFIER — its resource name with every `-` replaced
/// by `_`, a form `TelemetryPipelineName`'s pattern (`[a-z][a-z0-9\-]+`) admits no
/// pipeline name in. The metadata is not the configuration document, so a
/// `data_source_name` the document declares for such a source does not rename it. A
/// third-party source reports the configured name, or the plugin's own when the document
/// configures none.
///
/// That identifier is a function of the name THIS request carried and of nothing else, so
/// two pipelines whose names differ only in a trailing token report data sources that
/// differ in exactly that token.
///
/// The rows differ along what the rule generalises over: the kind of source (two
/// different AWS-vended ones, two different third-party ones), how many words the
/// pipeline's name separates (none, two, several) and which token ends it, how much of the
/// data source the document declares (nothing, a name, a name and a type, the
/// `data_source_name`/`data_source_type` pair on the plugin's own block) and in which
/// form, how many data sources it declares, and whether a declared name itself carries
/// the resource separator.
///
/// The same derivation feeds a LISTING's `TelemetryPipelineSummary.ConfigurationSummary`,
/// so every row is asserted on `ListTelemetryPipelines` as well as on the read.
#[tokio::test]
async fn a_data_source_is_named_the_way_its_source_kind_names_one() {
    let emulator = emulator();

    let cases = [
        (
            "a CloudWatch Logs source declaring no data source",
            "probe-pipe-one",
            "source:\n  cloudwatch_logs:\n    log_group: /aws/telemetry/in\n\
             sink:\n  - cloudwatch_logs: {}\n",
            json!([{ "Name": "probe_pipe_one", "Type": "default" }]),
        ),
        (
            "a pipeline name that separates no words at all",
            "probepipeline",
            "source:\n  cloudwatch_logs: {}\nsink:\n  - cloudwatch_logs: {}\n",
            json!([{ "Name": "probepipeline", "Type": "default" }]),
        ),
        (
            "a pipeline name separating several words",
            "sim2real-probe-pipe-a1b2",
            "pipeline:\n  source:\n    cloudwatch_logs: {}\n  \
             sink:\n    - cloudwatch_logs: {}\n",
            json!([{ "Name": "sim2real_probe_pipe_a1b2", "Type": "default" }]),
        ),
        (
            "the same pipeline shape under a name ending in a different token, which is \
             the only part of the data source's name that moves with it",
            "sim2real-probe-pipe-c7fd",
            "pipeline:\n  source:\n    cloudwatch_logs: {}\n  \
             sink:\n    - cloudwatch_logs: {}\n",
            json!([{ "Name": "sim2real_probe_pipe_c7fd", "Type": "default" }]),
        ),
        (
            "another vended source declaring no data source",
            "flow-logs-pipe",
            "source:\n  amazon_vpc_flow: {}\nsink:\n  - cloudwatch_logs: {}\n",
            json!([{ "Name": "flow_logs_pipe", "Type": "default" }]),
        ),
        (
            "a vended source declaring a data source whose own name carries the \
             resource separator, which does not rename the metadata's data source",
            "declared-feed-pipe",
            "source:\n  cloudwatch_logs:\n    data_sources:\n      - custom-feed\n\
             sink:\n  - cloudwatch_logs: {}\n",
            json!([{ "Name": "declared_feed_pipe", "Type": "default" }]),
        ),
        (
            "a vended source declaring a data source the vended-log schema knows by \
             name, which selects the feed rather than naming the data source",
            "trail-feed-pipe",
            "pipeline:\n  source:\n    cloudwatch_logs:\n      data_sources:\n        \
               - aws_cloudtrail\n  sink:\n    - cloudwatch_logs: {}\n",
            json!([{ "Name": "trail_feed_pipe", "Type": "default" }]),
        ),
        (
            "a vended source declaring the `data_source_name`/`data_source_type` pair on \
             its own block, whose declared name the metadata's own overrides",
            "paired-feed-pipe",
            "source:\n  cloudwatch_logs:\n    data_source_name: some_other_label\n    \
               data_source_type: custom\n\
             sink:\n  - cloudwatch_logs: {}\n",
            json!([{ "Name": "paired_feed_pipe", "Type": "custom" }]),
        ),
        (
            "a vended source listing two data sources, one summary entry per declared \
             data source, each named by the pipeline's own metadata",
            "two-feeds-pipe",
            "source:\n  cloudwatch_logs:\n    data_sources:\n      \
               - name: aws_cloudtrail\n      \
               - name: aws_security_hub_cspm\n        type: custom\n\
             sink:\n  - cloudwatch_logs: {}\n",
            json!([
                { "Name": "two_feeds_pipe", "Type": "default" },
                { "Name": "two_feeds_pipe", "Type": "custom" },
            ]),
        ),
        (
            "a third-party source declaring no data source, which defaults to the \
             plugin name",
            "third-party-pipe",
            "source:\n  s3:\n    bucket: telemetry-archive\n\
             sink:\n  - cloudwatch_logs: {}\n",
            json!([{ "Name": "s3" }]),
        ),
        (
            "a third-party source declaring its data source, which is reported exactly \
             as configured",
            "archive-feed-pipe",
            "source:\n  s3:\n    data_source_name: archive-logs\n\
             sink:\n  - cloudwatch_logs: {}\n",
            json!([{ "Name": "archive-logs" }]),
        ),
        (
            "another third-party source listing two data sources, each reported as \
             configured",
            "http-feeds-pipe",
            "source:\n  http:\n    data_sources:\n      - first-feed\n      - second-feed\n\
             sink:\n  - cloudwatch_logs: {}\n",
            json!([{ "Name": "first-feed" }, { "Name": "second-feed" }]),
        ),
    ];

    for (case, name, body, expected) in &cases {
        let (status, created) = call(
            &emulator,
            HOME_REGION,
            "/CreateTelemetryPipeline",
            &json!({ "Name": name, "Configuration": { "Body": body } }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: creating {name:?} must answer 200, got {status} {created:?}"
        );

        let (status, read) = call(
            &emulator,
            HOME_REGION,
            "/GetTelemetryPipeline",
            &json!({ "PipelineIdentifier": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading {name:?} must answer 200, got {status} {read:?}"
        );
        assert_eq!(
            read.pointer("/Pipeline/ConfigurationSummary/DataSources"),
            Some(expected),
            "case {case:?}: expected those data sources for {body:?}, got {read:?}"
        );
        // The identifier form is a DATA SOURCE name the service derives: the pipeline
        // resource keeps the name it was created with, which is the one a later read
        // takes, so the two forms never swap places.
        assert_eq!(
            read.pointer("/Pipeline/Name").and_then(Value::as_str),
            Some(*name),
            "case {case:?}: the pipeline keeps its own name, got {read:?}"
        );
    }

    // The listing derives its summaries from the same stored documents, so it reports
    // every row's data sources identically to the read of that pipeline.
    let (status, listed) = call(
        &emulator,
        HOME_REGION,
        "/ListTelemetryPipelines",
        &json!({}),
    )
    .await;
    assert_eq!(status, 200, "listing must answer 200, got {status} {listed:?}");
    let summaries = listed
        .get("PipelineSummaries")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the listing reports the pipelines, got {listed:?}"));
    for (case, name, body, expected) in &cases {
        let summary = summaries
            .iter()
            .find(|summary| summary.get("Name").and_then(Value::as_str) == Some(*name))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the listing reports {name:?}, got {listed:?}")
            });
        assert_eq!(
            summary.pointer("/ConfigurationSummary/DataSources"),
            Some(expected),
            "case {case:?}: the listing reports the same data sources for {body:?}, \
             got {summary:?}"
        );
    }
}

/// A pipeline's name as the metadata of the log events vended for it carries it: an
/// IDENTIFIER, which separates its words with `_` where a pipeline's resource name
/// separates them with `-`.
fn identifier_form(name: &str) -> String {
    name.replace('-', "_")
}

/// The sink the documents below write, so each of their rows differs only in its source.
const SINK: &str = "sink:\n  - cloudwatch_logs:\n      log_group: /aws/telemetry/out\n";

/// Create the pipeline `name` with `body`, answering the `DataSources` a read of it
/// reports.
async fn created_data_sources(
    emulator: &Emulator,
    case: &str,
    name: &str,
    body: &str,
) -> Value {
    let (status, created) = call(
        emulator,
        HOME_REGION,
        "/CreateTelemetryPipeline",
        &json!({ "Name": name, "Configuration": { "Body": body } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating {name:?} must answer 200, got {status} {created:?}"
    );
    read_data_sources(emulator, case, name).await
}

/// The `DataSources` a read of the pipeline `name` reports.
async fn read_data_sources(emulator: &Emulator, case: &str, name: &str) -> Value {
    let (status, read) = call(
        emulator,
        HOME_REGION,
        "/GetTelemetryPipeline",
        &json!({ "PipelineIdentifier": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {name:?} must answer 200, got {status} {read:?}"
    );
    read.pointer("/Pipeline/ConfigurationSummary/DataSources")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the read reports the pipeline's data sources, got {read:?}")
        })
}

/// Every `Name` of a reported `DataSources` list.
fn reported_names(case: &str, data_sources: &Value) -> Vec<String> {
    data_sources
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: DataSources is a list, got {data_sources:?}"))
        .iter()
        .map(|entry| {
            entry
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or("<no Name>")
                .to_string()
        })
        .collect()
}

/// Rule 3, for a source whose document DOES declare a `data_source_name`: the declared
/// label names a vended source's data source not at all, and names a third-party
/// source's exactly.
///
/// The rows differ along what the rule generalises over: the source plugin (each of the
/// AWS-vended ones the schema carries a branch for, two different third-party ones),
/// where the document declares the name (on the plugin's own block, as a bare entry of
/// its `data_sources` list, as a `name`/`type` mapping in that list) and whether the
/// declared token is in identifier or resource form, how many data sources the document
/// declares, how many words the pipeline's name separates, and whether the source
/// carries a property of its own beside the declaration.
#[tokio::test]
async fn a_declared_data_source_name_governs_only_a_third_party_source() {
    let emulator = emulator();

    // (case, pipeline name, document, the data_source_name the document declares,
    //  whether the source reads AWS-vended log events)
    for (case, name, body, declared, vended) in [
        (
            "a vended CloudWatch Logs source declaring a name on the plugin's own block",
            "vended-trail-pipe-3cf4",
            format!(
                "source:\n  cloudwatch_logs:\n    data_source_name: other_label_c7fd\n    \
                   log_group: /aws/telemetry/in\n{SINK}"
            ),
            "other_label_c7fd",
            true,
        ),
        (
            "the same document under a pipeline name ending in another token, the only \
             part of the reported name that moves with it",
            "vended-trail-pipe-9ab7",
            format!(
                "source:\n  cloudwatch_logs:\n    data_source_name: other_label_c7fd\n    \
                   log_group: /aws/telemetry/in\n{SINK}"
            ),
            "other_label_c7fd",
            true,
        ),
        (
            "a vended source declaring, as a name/type mapping, a label in resource form",
            "vended-mapped-pipe",
            format!(
                "source:\n  cloudwatch_logs:\n    data_sources:\n      \
                   - name: custom-feed\n        type: custom\n{SINK}"
            ),
            "custom-feed",
            true,
        ),
        (
            "another vended plugin, declaring a label of its own",
            "flowlog-pipe-7e21",
            format!("source:\n  amazon_vpc_flow:\n    data_source_name: flow_label\n{SINK}"),
            "flow_label",
            true,
        ),
        (
            "a third vended plugin, carrying a property of its own beside the label",
            "cdn-pipe-two",
            format!(
                "source:\n  amazon_cloudfront:\n    data_source_name: cdn_label\n    \
                   cloudfront:\n      distribution_id: E1EXAMPLE\n{SINK}"
            ),
            "cdn_label",
            true,
        ),
        (
            "a fourth vended plugin, declaring its label as a bare list entry",
            "dns-pipe-one",
            "pipeline:\n  source:\n    amazon_route53:\n      data_sources:\n        \
               - dns-feed\n  sink:\n    - cloudwatch_logs: {}\n"
                .to_string(),
            "dns-feed",
            true,
        ),
        (
            "a third-party source declaring its data source, which is reported as \
             configured",
            "archive-pipe-s3",
            format!("source:\n  s3:\n    data_source_name: archive-logs\n{SINK}"),
            "archive-logs",
            false,
        ),
        (
            "another third-party source, declaring two data sources as bare list entries",
            "feed-pipe-http",
            format!(
                "source:\n  http:\n    data_sources:\n      - first-feed\n      \
                   - second-feed\n{SINK}"
            ),
            "second-feed",
            false,
        ),
    ] {
        let data_sources = created_data_sources(&emulator, case, name, &body).await;
        let reported = reported_names(case, &data_sources);
        if vended {
            let expected = identifier_form(name);
            assert!(
                reported == vec![expected.clone()],
                "case {case:?}: a vended source's data source is the pipeline's own \
                 {expected:?}, got {reported:?}"
            );
            assert!(
                !reported.iter().any(|reported| *reported == declared),
                "case {case:?}: the document's label {declared:?} names no vended data \
                 source, got {reported:?}"
            );
        } else {
            assert!(
                reported.iter().any(|reported| *reported == declared),
                "case {case:?}: a third-party source is reported under its configured \
                 {declared:?}, got {reported:?}"
            );
            assert!(
                !reported.contains(&identifier_form(name)),
                "case {case:?}: a third-party source is not named after the pipeline, \
                 got {reported:?}"
            );
        }
    }
}

/// Rule 3, across an UPDATE: re-pointing the stored document at another
/// `data_source_name` moves a third-party source's reported name and leaves a vended
/// one's where the pipeline's own name puts it, since only the third-party half of the
/// split reads that declaration.
///
/// The two rows are the two kinds of source, each updated between documents that differ
/// only in the name they declare — with the stored body asserted to have changed under
/// the second read, so neither row can pass on a summary derived from the first
/// document.
#[tokio::test]
async fn an_update_moves_only_the_data_source_name_the_document_governs() {
    let emulator = emulator();

    for (case, name, first, second, expected_before, expected_after) in [
        (
            "a vended source, whose reported name the pipeline's own metadata fixes",
            "renamed-vended-pipe",
            format!("source:\n  cloudwatch_logs:\n    data_source_name: first_label\n{SINK}"),
            format!("source:\n  cloudwatch_logs:\n    data_source_name: second_label\n{SINK}"),
            json!([{ "Name": "renamed_vended_pipe", "Type": "default" }]),
            json!([{ "Name": "renamed_vended_pipe", "Type": "default" }]),
        ),
        (
            "a third-party source, whose reported name the document governs",
            "renamed-third-pipe",
            format!("source:\n  s3:\n    data_source_name: first-feed\n{SINK}"),
            format!("source:\n  s3:\n    data_source_name: second-feed\n{SINK}"),
            json!([{ "Name": "first-feed" }]),
            json!([{ "Name": "second-feed" }]),
        ),
    ] {
        let before = created_data_sources(&emulator, case, name, &first).await;
        assert_eq!(
            before, expected_before,
            "case {case:?}: expected those data sources for {first:?}, got {before:?}"
        );

        let (status, updated) = call(
            &emulator,
            HOME_REGION,
            "/UpdateTelemetryPipeline",
            &json!({
                "PipelineIdentifier": name,
                "Configuration": { "Body": second },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: updating {name:?} must answer 200, got {status} {updated:?}"
        );

        let (status, read) = call(
            &emulator,
            HOME_REGION,
            "/GetTelemetryPipeline",
            &json!({ "PipelineIdentifier": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading {name:?} must answer 200, got {status} {read:?}"
        );
        // The update really landed, so the second read's summary is derived from the
        // second document rather than from the first.
        assert_eq!(
            read.pointer("/Pipeline/Configuration/Body")
                .and_then(Value::as_str),
            Some(second.as_str()),
            "case {case:?}: the read reports the updated document, got {read:?}"
        );
        assert_eq!(
            read.pointer("/Pipeline/ConfigurationSummary/DataSources"),
            Some(&expected_after),
            "case {case:?}: expected those data sources for {second:?}, got {read:?}"
        );
    }
}
