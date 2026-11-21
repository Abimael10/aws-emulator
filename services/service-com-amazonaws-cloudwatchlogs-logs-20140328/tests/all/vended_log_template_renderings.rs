// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a vended-log configuration template says a delivery of its log
//! type may be WRITTEN AS, and for the two parts of a record a template publishes as optional.
//!
//! The rules under test:
//!
//! * `allowedOutputFormats` is "the allowed output formats" of a delivery, so a rendering is
//!   published where the DESTINATION can store it and the RECORD has something to say in it: a
//!   destination that receives log records takes the two per-record renderings (the record as
//!   the delivered line and the record as a structured document), a delivery that writes objects
//!   takes those plus the two file layouts (the headed text file and the columnar one, both
//!   properties of a file rather than of a record), the X-Ray trace destination receives spans
//!   as structured documents, and a log type whose record IS one document is on offer as that
//!   document alone. `allowedFieldDelimiters` — "the valid values that a caller can use as field
//!   delimiters" — and `defaultDeliveryConfigValues.fieldDelimiter` are published exactly where
//!   one of the delimited renderings is.
//! * `mandatory` says "the record field must be present in the recordFields parameter", which is
//!   a statement about the record: the vended-log envelope a sender attributes its records with
//!   (the record's own ingestion timestamp, the resource the record came from and the instant of
//!   the event) is not such a field, so a sender whose record opens with that envelope publishes
//!   every part of it as optional.
//! * the bare id of the sending resource and the suffix-path variable naming it are the same
//!   identifier, so a sender that publishes no variable naming the sending resource publishes no
//!   bare `resource_id` field either.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so what is
//! asserted here is what a client reads off the wire.
//!
//! The tables span what the rules generalise over rather than the senders one report named: each
//! destination type the model declares, senders whose records are a generic vended-log envelope,
//! a service-specific event family, a fixed-shape line and a single document, and — for the two
//! record rules — the WHOLE published catalogue rather than a row of it.

use serde_json::{json, Value};

use crate::harness::Logs;

/// The operations under test, and the two a new row of the catalogue is read beside.
const DESCRIBE: &str = "DescribeConfigurationTemplates";
const PUT_SOURCE: &str = "PutDeliverySource";
const DESCRIBE_SOURCES: &str = "DescribeDeliverySources";

/// The largest page the model admits (`DescribeLimit` is `range 1..50`).
const MAX_LIMIT: usize = 50;

/// The `DeliveryDestinationType`s the model's enum names, in the order it DECLARES them
/// (`smithy_query(query="shape",shape="DeliveryDestinationType")`), and the one of them that
/// receives traces rather than log records.
const DESTINATION_TYPE_ORDER: [&str; 4] = ["S3", "CWL", "FH", "XRAY"];
const TRACE_DESTINATION_TYPE: &str = "XRAY";
const OBJECT_DESTINATION_TYPE: &str = "S3";

/// The `OutputFormat`s the model's enum names
/// (`smithy_query(query="shape",shape="OutputFormat")`), the two that are PER-RECORD renderings,
/// the two that are FILE layouts, and the ones that write a record as a delimited line.
const PER_RECORD_FORMATS: [&str; 2] = ["plain", "json"];
const FILE_FORMATS: [&str; 2] = ["w3c", "parquet"];
const STRUCTURED_FORMAT: &str = "json";
const DELIMITED_FORMATS: [&str; 3] = ["plain", "raw", "w3c"];

/// What the operation publishes for a delivery that can be written in a delimited rendering, and
/// the delimiter such a delivery is given when it names none: `FieldDelimiter` is modelled
/// `@length 0..5`, so NO delimiter is a value in its own right.
const FIELD_DELIMITERS: [&str; 3] = ["\t", " ", ","];
const DEFAULT_FIELD_DELIMITER: &str = "";

/// The members of the vended-log envelope a sender attributes its records with: the record's own
/// ingestion timestamp, the resource the record came from — its ARN, its bare id, or the
/// identifier a sender names its own kind of resource by — and the instant of the event, in the
/// time unit that sender publishes.
///
/// Stated here rather than imported from the catalogue, so an edit to either the published
/// envelope or the flags it carries fails this suite instead of agreeing with itself.
const ENVELOPE_ATTRIBUTION_FIELDS: [&str; 3] = ["resource_arn", "organization_arn", "application_id"];
const ENVELOPE_FIELDS: [&str; 7] = [
    "timestamp",
    "resource_arn",
    "resource_id",
    "organization_arn",
    "application_id",
    "event_timestamp",
    "message_timestamp_ms",
];

/// The bare id of the sending resource, and the suffix-path variables that name something other
/// than the sending resource: the account and the Region a delivery is made in, and the time
/// partition of the object it writes.
const RESOURCE_ID_FIELD: &str = "resource_id";
const NON_RESOURCE_SUFFIX_PATH_FIELDS: [&str; 6] =
    ["accountid", "region", "yyyy", "MM", "dd", "HH"];

/// One resource of Amazon CodeWhisperer whose logs a delivery source can represent, and the one
/// log type that sender sends ("For Amazon CodeWhisperer, the valid value is EVENT_LOGS",
/// `smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`).
const CUSTOMIZATION_ARN: &str =
    "arn:aws:codewhisperer:eu-west-2:123456789012:customization/EXAMPLEcustomization";
const CUSTOMIZATION_LOG_TYPE: &str = "EVENT_LOGS";

/// Every template of one (service, log type, resource type) slice, in the order the operation
/// publishes them.
async fn slice(logs: &Logs, service: &str, log_type: &str, resource_type: &str) -> Vec<Value> {
    let body = json!({
        "service": service,
        "logTypes": [log_type],
        "resourceTypes": [resource_type],
        "limit": MAX_LIMIT,
    });
    let response = logs.ok(DESCRIBE, &body).await;
    response
        .get("configurationTemplates")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {body}: the response carries configurationTemplates, got {response}")
        })
        .clone()
}

/// Every template a filter selects, walked page by page.
async fn walk(logs: &Logs, filter: &Value) -> Vec<Value> {
    let mut templates: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = filter.clone();
        body["limit"] = json!(MAX_LIMIT);
        if let Some(token) = &token {
            body["nextToken"] = json!(token);
        }
        let response = logs.ok(DESCRIBE, &body).await;
        let page = response
            .get("configurationTemplates")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {body}: the response carries configurationTemplates, got {response}")
            })
            .clone();
        templates.extend(page);
        match response.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    templates
}

/// A string member of a reported template.
fn member(template: &Value, name: &str) -> String {
    template
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a reported template carries {name}, got {template}"))
        .to_string()
}

/// What identifies one reported template.
fn key_of(template: &Value) -> String {
    [
        member(template, "service"),
        member(template, "logType"),
        member(template, "resourceType"),
        member(template, "deliveryDestinationType"),
    ]
    .join("|")
}

/// A list-of-strings member of a reported template, or `None` when it carries none.
fn strings(template: &Value, name: &str) -> Option<Vec<String>> {
    template.get(name).map(|value| {
        value
            .as_array()
            .unwrap_or_else(|| panic!("{name} holds a list, got {value}"))
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .unwrap_or_else(|| panic!("{name} holds strings, got {entry}"))
                    .to_string()
            })
            .collect()
    })
}

/// The `allowedFields` of a reported template, as (name, mandatory) pairs.
fn allowed_fields(template: &Value) -> Vec<(String, bool)> {
    template
        .get("allowedFields")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("a template carries allowedFields, got {template}"))
        .iter()
        .map(|field| {
            (
                member(field, "name"),
                field
                    .get("mandatory")
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| {
                        panic!("an allowed field says whether it is mandatory, got {field}")
                    }),
            )
        })
        .collect()
}

/// The renderings a template publishes are the ones its destination can store, narrowed to the
/// ones its record has something to say in — and the delimiters to choose from, with the one a
/// delivery that names none is given, are published exactly where a delimited rendering is.
#[tokio::test]
async fn a_templates_renderings_are_its_destinations_narrowed_by_its_record() {
    let logs = Logs::new();

    // (service, log type, resource type, whether its record IS one document): senders whose
    // record is a generic vended-log envelope, one of named fields of its own, a fixed-shape
    // line, a service-specific event family, a log type that reaches the trace destination as
    // well, and the one sender whose record is a single document.
    let cases: [(&str, &str, &str, bool); 7] = [
        ("apigateway", "EXECUTION_LOGS", "stages", false),
        ("qbusiness", "EVENT_LOGS", "application", false),
        ("amplify", "ACCESS_LOGS", "app", false),
        ("elasticloadbalancing", "NLB_ACCESS_LOGS", "loadbalancer", false),
        ("mediatailor", "MANIFEST_SERVICE_LOGS", "playbackConfiguration", false),
        ("bedrock-agentcore", "TRACES", "runtime", false),
        ("securityhub", "SECURITY_FINDING_LOGS", "hubv2", true),
    ];

    for (service, log_type, resource_type, document_record) in cases {
        let templates = slice(&logs, service, log_type, resource_type).await;
        assert!(
            !templates.is_empty(),
            "case {service}|{log_type}|{resource_type}: the catalogue publishes templates for this log source, got none"
        );
        for template in &templates {
            let case = key_of(template);
            let destination_type = member(template, "deliveryDestinationType");
            let formats = strings(template, "allowedOutputFormats")
                .unwrap_or_else(|| panic!("case {case}: a template carries allowedOutputFormats"));

            let expected: Vec<String> = if destination_type == TRACE_DESTINATION_TYPE {
                vec![STRUCTURED_FORMAT.to_string()]
            } else if document_record {
                vec![PER_RECORD_FORMATS[0].to_string()]
            } else if destination_type == OBJECT_DESTINATION_TYPE {
                PER_RECORD_FORMATS
                    .iter()
                    .chain(FILE_FORMATS.iter())
                    .map(|format| (*format).to_string())
                    .collect()
            } else {
                PER_RECORD_FORMATS
                    .iter()
                    .map(|format| (*format).to_string())
                    .collect()
            };
            assert_eq!(
                formats, expected,
                "case {case}: expected the renderings {expected:?}, got {formats:?}"
            );

            // The delimiters: published exactly where a delimited rendering is, and the default
            // is published with them — no delimiter at all.
            let delimited = formats
                .iter()
                .any(|format| DELIMITED_FORMATS.contains(&format.as_str()));
            let delimiters = strings(template, "allowedFieldDelimiters");
            let default_delimiter = template
                .get("defaultDeliveryConfigValues")
                .and_then(|defaults| defaults.get("fieldDelimiter"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let published: Option<Vec<String>> = delimited.then(|| {
                FIELD_DELIMITERS
                    .iter()
                    .map(|delimiter| (*delimiter).to_string())
                    .collect()
            });
            assert_eq!(
                delimiters, published,
                "case {case}: expected the delimiters {published:?}, got {delimiters:?}"
            );
            assert_eq!(
                default_delimiter.as_deref(),
                delimited.then_some(DEFAULT_FIELD_DELIMITER),
                "case {case}: expected the default delimiter {DEFAULT_FIELD_DELIMITER:?} exactly \
                 where a delimited rendering is published, got {default_delimiter:?}"
            );
        }
    }
}

/// The same rule over the WHOLE catalogue, from the other side: every destination type the model
/// declares publishes the renderings it can store, so the file layouts appear only where a
/// delivery writes objects and both per-record renderings appear everywhere a record is received.
#[tokio::test]
async fn every_destination_type_publishes_the_renderings_it_can_store() {
    let logs = Logs::new();

    for destination_type in DESTINATION_TYPE_ORDER {
        let templates = walk(&logs, &json!({"deliveryDestinationTypes": [destination_type]})).await;
        assert!(
            !templates.is_empty(),
            "case {destination_type}: every destination type the model names has templates, got none"
        );
        for template in &templates {
            let case = key_of(template);
            let formats = strings(template, "allowedOutputFormats")
                .unwrap_or_else(|| panic!("case {case}: a template carries allowedOutputFormats"));
            assert!(
                !formats.is_empty(),
                "case {case}: a template publishes at least one rendering, got {formats:?}"
            );
            for format in &FILE_FORMATS {
                assert!(
                    !formats.contains(&(*format).to_string())
                        || destination_type == OBJECT_DESTINATION_TYPE,
                    "case {case}: the file layout {format:?} is published only where a delivery writes objects, got {formats:?}"
                );
            }
            if destination_type != TRACE_DESTINATION_TYPE {
                assert!(
                    formats.contains(&PER_RECORD_FORMATS[0].to_string()),
                    "case {case}: a destination that receives records takes the record as the delivered line, got {formats:?}"
                );
            }
        }
    }
}

/// The vended-log envelope is published as OPTIONAL: a sender whose record is attributed with
/// the resource it came from marks no part of that envelope mandatory — checked over the whole
/// published catalogue, not one row of it.
#[tokio::test]
async fn the_vended_log_envelope_is_published_as_optional_over_the_whole_catalogue() {
    let logs = Logs::new();
    let catalogue = walk(&logs, &json!({})).await;
    assert!(
        catalogue.len() > MAX_LIMIT,
        "the whole catalogue is under test, got {} templates",
        catalogue.len()
    );

    let mut enveloped = 0usize;
    let mut with_mandatory_fields = 0usize;
    for template in &catalogue {
        let case = key_of(template);
        let fields = allowed_fields(template);
        let attributed = fields
            .iter()
            .any(|(name, _)| ENVELOPE_ATTRIBUTION_FIELDS.contains(&name.as_str()));
        if fields.iter().any(|(_, mandatory)| *mandatory) {
            with_mandatory_fields += 1;
        }
        if !attributed {
            continue;
        }
        enveloped += 1;
        for (name, mandatory) in &fields {
            assert!(
                !(*mandatory && ENVELOPE_FIELDS.contains(&name.as_str())),
                "case {case}: the envelope field {name:?} is published as optional, got mandatory"
            );
        }
    }
    assert!(
        enveloped > 0,
        "the catalogue publishes senders whose records carry the vended-log envelope, got none"
    );
    assert!(
        with_mandatory_fields > 0,
        "the catalogue publishes records a delivery may not drop a field of, got none"
    );
}

/// The bare id of the sending resource and the suffix-path variable naming it are the same
/// identifier: a sender that publishes no variable naming the sending resource publishes no bare
/// `resource_id` field either — checked over every object-destination template the catalogue
/// holds.
#[tokio::test]
async fn a_sender_naming_no_resource_variable_publishes_no_bare_resource_id() {
    let logs = Logs::new();
    let templates = walk(
        &logs,
        &json!({"deliveryDestinationTypes": [OBJECT_DESTINATION_TYPE]}),
    )
    .await;
    assert!(
        !templates.is_empty(),
        "the object-destination templates are under test, got none"
    );

    let mut naming_a_resource = 0usize;
    let mut naming_none = 0usize;
    for template in &templates {
        let case = key_of(template);
        let suffix_fields = strings(template, "allowedSuffixPathFields")
            .unwrap_or_else(|| panic!("case {case}: an object template carries allowedSuffixPathFields"));
        let names_a_resource = suffix_fields
            .iter()
            .any(|field| !NON_RESOURCE_SUFFIX_PATH_FIELDS.contains(&field.as_str()));
        let publishes_resource_id = allowed_fields(template)
            .iter()
            .any(|(name, _)| name == RESOURCE_ID_FIELD);
        if names_a_resource {
            naming_a_resource += 1;
        } else {
            naming_none += 1;
            assert!(
                !publishes_resource_id,
                "case {case}: this sender names no resource variable in {suffix_fields:?}, so it publishes no {RESOURCE_ID_FIELD:?} field"
            );
        }
    }
    assert!(
        naming_a_resource > 0 && naming_none > 0,
        "both kinds of sender are under test, got {naming_a_resource} naming a resource and {naming_none} naming none"
    );
}

/// The Amazon CodeWhisperer customization templates are published, and a delivery source over a
/// customization can be created from what they say: the catalogue row and the log types a put is
/// judged against are the same catalogue.
#[tokio::test]
async fn the_codewhisperer_customization_templates_are_published_and_usable() {
    let logs = Logs::new();

    let templates = walk(&logs, &json!({"service": "codewhisperer"})).await;
    let reported: Vec<String> = templates.iter().map(key_of).collect();
    let expected: Vec<String> = DESTINATION_TYPE_ORDER
        .iter()
        .filter(|destination_type| **destination_type != TRACE_DESTINATION_TYPE)
        .map(|destination_type| {
            format!("codewhisperer|{CUSTOMIZATION_LOG_TYPE}|customization|{destination_type}")
        })
        .collect();
    assert_eq!(
        reported, expected,
        "case service codewhisperer: expected the templates {expected:?}, got {reported:?}"
    );

    let object_template = templates
        .iter()
        .find(|template| {
            member(template, "deliveryDestinationType") == OBJECT_DESTINATION_TYPE
        })
        .expect("the catalogue publishes an object-destination template for a customization");
    let suffix_fields = strings(object_template, "allowedSuffixPathFields")
        .expect("an object template carries allowedSuffixPathFields");
    assert_eq!(
        suffix_fields,
        vec![
            "accountid".to_string(),
            "region".to_string(),
            "customization_id".to_string(),
            "yyyy".to_string(),
            "MM".to_string(),
            "dd".to_string(),
            "HH".to_string(),
        ],
        "case codewhisperer object template: expected the published suffix path variables, got {suffix_fields:?}"
    );
    let suffix_path = object_template
        .get("defaultDeliveryConfigValues")
        .and_then(|defaults| defaults.get("s3DeliveryConfiguration"))
        .and_then(|s3| s3.get("suffixPath"))
        .and_then(Value::as_str)
        .expect("an object template carries a default suffix path");
    assert_eq!(
        suffix_path, "{region}/{customization_id}/{yyyy}/{MM}/{dd}/{HH}/",
        "case codewhisperer object template: expected the published default suffix path, got {suffix_path:?}"
    );
    let action = member(
        object_template,
        "allowedActionForAllowVendedLogsDeliveryForResource",
    );
    assert_eq!(
        action, "codewhisperer:AllowVendedLogDeliveryForResource",
        "case codewhisperer object template: expected the sender's own delivery action, got {action:?}"
    );

    // A delivery source over a customization is accepted for the log type that row publishes,
    // and reads back as the resource it represents.
    logs.ok(
        PUT_SOURCE,
        &json!({
            "name": "customization-source",
            "resourceArn": CUSTOMIZATION_ARN,
            "logType": CUSTOMIZATION_LOG_TYPE,
        }),
    )
    .await;
    let sources = logs.ok(DESCRIBE_SOURCES, &json!({"limit": MAX_LIMIT})).await;
    let stored = sources
        .get("deliverySources")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the response carries deliverySources, got {sources}"))
        .iter()
        .find(|source| source.get("name").and_then(Value::as_str) == Some("customization-source"))
        .unwrap_or_else(|| panic!("the stored source reads back, got {sources}"));
    assert_eq!(
        stored.get("logType").and_then(Value::as_str),
        Some(CUSTOMIZATION_LOG_TYPE),
        "case a customization source: expected the log type it was created with, got {stored}"
    );
    assert_eq!(
        stored.get("service").and_then(Value::as_str),
        Some("codewhisperer"),
        "case a customization source: expected the sending service of its resource, got {stored}"
    );

    // A log type that sender does not send is refused, so the row is a catalogue rather than a
    // free pass for the service.
    let code = logs
        .err(
            PUT_SOURCE,
            &json!({
                "name": "customization-wrong-type",
                "resourceArn": CUSTOMIZATION_ARN,
                "logType": "ACCESS_LOGS",
            }),
        )
        .await;
    assert_eq!(
        code, "ValidationException",
        "case a log type codewhisperer does not send: expected ValidationException, got {code}"
    );
}
