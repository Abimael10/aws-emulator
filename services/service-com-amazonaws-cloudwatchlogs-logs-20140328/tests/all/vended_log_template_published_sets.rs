// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT THE PUBLISHED CATALOGUE CONTAINS: which (service, resource type,
//! log type) combinations `DescribeConfigurationTemplates` publishes a template for, which
//! template a sender's slice opens with, and the record and packaging a sender publishes where
//! those are its own rather than the shape every sender shares.
//!
//! The rules under test:
//!
//! * a sender's templates are the log types THAT KIND OF RESOURCE SENDS and no others — the
//!   catalogue the `logType` member of `PutDeliverySourceRequest` publishes per sender
//!   (`smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`) — so a log
//!   type that sender does not send has no template at any delivery destination type, the X-Ray
//!   trace destination included;
//! * a delivery to the destination that receives a STREAM of records can be written as the
//!   record the sender sent — `OutputFormat` names that rendering
//!   (`smithy_query(query="shape",shape="OutputFormat")`) and the field-delimiter member is
//!   about "a delivery that delivers in Plain, W3C, or Raw format" — while a destination that
//!   STORES each record as its own item takes the per-record renderings alone;
//! * the record of a log type, the fields a delivery of it carries when it names none, the
//!   columnar file layout of its delivered objects and the S3 Tables datasource it binds to are
//!   the SENDER's own published data, so each is reported as that sender publishes it rather
//!   than as a shape derived from the other senders';
//! * the catalogue's element ORDER is published data too: a sender's slice opens with the
//!   template that sender publishes first, and a delivery created from the slice takes its
//!   defaults from it.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so what is
//! asserted here is what a client reads off the wire.
//!
//! The tables span what the rules generalise over rather than the senders one report named:
//! senders of one log type and of eleven, senders that send traces and senders that do not,
//! each destination type the model declares, and records that are a fixed-shape line, an
//! all-mandatory event document and a sender's own named fields.

use serde_json::{json, Value};

use crate::harness::Logs;

/// The operations under test, and the ones a delivery built from the catalogue goes through.
const DESCRIBE: &str = "DescribeConfigurationTemplates";
const PUT_SOURCE: &str = "PutDeliverySource";
const PUT_DESTINATION: &str = "PutDeliveryDestination";
const CREATE_DELIVERY: &str = "CreateDelivery";
const GET_DELIVERY: &str = "GetDelivery";

/// The largest page the model admits (`DescribeLimit` is `range 1..50`).
const MAX_LIMIT: usize = 50;

/// The `DeliveryDestinationType`s the model's enum names, in the order it DECLARES them
/// (`smithy_query(query="shape",shape="DeliveryDestinationType")`), the one that receives traces
/// rather than a record of fields — which the catalogue publishes no template for — the one that
/// receives a stream of records, the one that writes objects, and the one that stores each
/// record as its own item.
const DESTINATION_TYPE_ORDER: [&str; 3] = ["S3", "CWL", "FH"];
const TRACE_DESTINATION_TYPE: &str = "XRAY";
const STREAM_DESTINATION_TYPE: &str = "FH";
const OBJECT_DESTINATION_TYPE: &str = "S3";
const RECORD_DESTINATION_TYPE: &str = "CWL";

/// The `OutputFormat`s this suite names, as the model's enum spells them: the record as the
/// delivered line, the record as a structured document, the record as the sender sent it, the
/// headed text file and the columnar file.
const PLAIN_FORMAT: &str = "plain";
const STRUCTURED_FORMAT: &str = "json";
const RAW_FORMAT: &str = "raw";
const HEADED_FILE_FORMAT: &str = "w3c";
const COLUMNAR_FORMAT: &str = "parquet";

/// The `logType` of a source that sends traces, as the `logType` member's own catalogue spells
/// it.
const TRACE_LOG_TYPE: &str = "TRACES";

/// One sender the catalogue publishes a row for: the log types that kind of resource sends, and
/// log types it does NOT send — each of them one some OTHER row of the same service, or of
/// another service, does send, so a template for it would be a capability the sender does not
/// have.
///
/// Source: the `logType` member of `PutDeliverySourceRequest`
/// (`smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`), which names
/// the valid log types one sender at a time: "For Amazon Bedrock Agents, the valid values are
/// APPLICATION_LOGS and EVENT_LOGS. [...] For Amazon Bedrock Knowledge Bases, the valid values
/// are APPLICATION_LOGS and TRACES."
struct Sender {
    service: &'static str,
    resource_type: &'static str,
    sends: &'static [&'static str],
    does_not_send: &'static [&'static str],
}

const SENDERS: &[Sender] = &[
    Sender {
        service: "bedrock",
        resource_type: "agent-alias",
        sends: &["EVENT_LOGS", "APPLICATION_LOGS"],
        does_not_send: &[TRACE_LOG_TYPE, "USAGE_LOGS"],
    },
    Sender {
        service: "bedrock",
        resource_type: "knowledge-base",
        sends: &["APPLICATION_LOGS", TRACE_LOG_TYPE],
        does_not_send: &["EVENT_LOGS"],
    },
    Sender {
        service: "bedrock-agentcore",
        resource_type: "runtime",
        sends: &["APPLICATION_LOGS", "USAGE_LOGS", TRACE_LOG_TYPE],
        does_not_send: &["EVENT_LOGS"],
    },
    Sender {
        service: "bedrock-agentcore",
        resource_type: "workload-identity",
        sends: &["APPLICATION_LOGS", TRACE_LOG_TYPE],
        does_not_send: &["USAGE_LOGS"],
    },
    Sender {
        service: "amplify",
        resource_type: "app",
        sends: &["ACCESS_LOGS", "WAF_LOGS"],
        does_not_send: &["APPLICATION_LOGS"],
    },
    Sender {
        service: "elasticloadbalancing",
        resource_type: "loadbalancer",
        sends: &[
            "ALB_CONNECTION_LOGS",
            "ALB_ACCESS_LOGS",
            "ALB_HEALTH_CHECK_LOGS",
            "NLB_ACCESS_LOGS",
        ],
        does_not_send: &["ACCESS_LOGS", TRACE_LOG_TYPE],
    },
    // A sender of eleven log types: the four Amazon EKS Auto Mode logs and the seven capability
    // logs the `logType` member's catalogue names.
    Sender {
        service: "eks",
        resource_type: "cluster",
        sends: &[
            "AUTO_MODE_LOAD_BALANCING_LOGS",
            "AUTO_MODE_BLOCK_STORAGE_LOGS",
            "AUTO_MODE_COMPUTE_LOGS",
            "AUTO_MODE_IPAM_LOGS",
            "EKS_CAPABILITY_ACK_LOGS",
            "EKS_CAPABILITY_ARGOCD_APPLICATION_LOGS",
            "EKS_CAPABILITY_ARGOCD_APPLICATIONSET_LOGS",
            "EKS_CAPABILITY_ARGOCD_COMMITSERVER_LOGS",
            "EKS_CAPABILITY_ARGOCD_REPOSERVER_LOGS",
            "EKS_CAPABILITY_ARGOCD_SERVER_LOGS",
            "EKS_CAPABILITY_KRO_LOGS",
        ],
        does_not_send: &["ACTION_LOGS", TRACE_LOG_TYPE],
    },
    // A sender of ONE log type: "For Amazon CodeWhisperer, the valid value is EVENT_LOGS."
    Sender {
        service: "codewhisperer",
        resource_type: "customization",
        sends: &["EVENT_LOGS"],
        does_not_send: &["APPLICATION_LOGS", "ACCESS_LOGS", TRACE_LOG_TYPE],
    },
    Sender {
        service: "qbusiness",
        resource_type: "application",
        sends: &["EVENT_LOGS", "SYNC_JOB_LOGS"],
        does_not_send: &["APPLICATION_LOGS"],
    },
    Sender {
        service: "workmail",
        resource_type: "organization",
        sends: &[
            "ACCESS_CONTROL_LOGS",
            "AUTHENTICATION_LOGS",
            "WORKMAIL_AVAILABILITY_PROVIDER_LOGS",
            "WORKMAIL_MAILBOX_ACCESS_LOGS",
            "WORKMAIL_PERSONAL_ACCESS_TOKEN_LOGS",
        ],
        does_not_send: &["EVENT_LOGS", TRACE_LOG_TYPE],
    },
];

/// One log source whose record and object layout are its SENDER's own published data, as a row
/// of the table below: the case it stands for, the service, the kind of resource and the log
/// type it keys, the allowed fields with their mandatory flags, the default record fields, the
/// renderings its OBJECT template publishes, and the S3 Tables datasource the log type binds to
/// (`None` where it binds to none).
type Published = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, bool)],
    &'static [&'static str],
    &'static [&'static str],
    Option<(&'static str, &'static str)>,
);

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
        templates.extend(
            response
                .get("configurationTemplates")
                .and_then(Value::as_array)
                .unwrap_or_else(|| {
                    panic!("case {body}: the response carries configurationTemplates, got {response}")
                })
                .clone(),
        );
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

/// A list-of-strings member of a reported template, or `None` where it carries none.
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

/// The `defaultDeliveryConfigValues.recordFields` of a reported template.
fn default_record_fields(template: &Value) -> Vec<String> {
    let defaults = template
        .get("defaultDeliveryConfigValues")
        .unwrap_or_else(|| panic!("a template carries defaultDeliveryConfigValues, got {template}"));
    strings(defaults, "recordFields")
        .unwrap_or_else(|| panic!("a template carries default recordFields, got {template}"))
}

/// The templates of one (service, resource type) row, in the order the operation publishes them.
async fn row(logs: &Logs, service: &str, resource_type: &str) -> Vec<Value> {
    walk(
        logs,
        &json!({"service": service, "resourceTypes": [resource_type]}),
    )
    .await
}

/// A sender publishes templates for the log types that kind of resource sends, and for no
/// others — at every delivery destination type, the trace destination included.
#[tokio::test]
async fn a_sender_publishes_the_log_types_that_kind_of_resource_sends_and_no_others() {
    let logs = Logs::new();

    for sender in SENDERS {
        let case = format!("{}|{}", sender.service, sender.resource_type);
        let templates = row(&logs, sender.service, sender.resource_type).await;
        assert!(
            !templates.is_empty(),
            "case {case}: the catalogue publishes templates for this sender, got none"
        );

        // The log types it publishes, each once, in the order the catalogue states them.
        let mut published: Vec<String> = Vec::new();
        for template in &templates {
            let log_type = member(template, "logType");
            if !published.contains(&log_type) {
                published.push(log_type);
            }
        }
        let mut sorted = published.clone();
        sorted.sort_unstable();
        let mut expected: Vec<String> = sender.sends.iter().map(|sent| (*sent).to_string()).collect();
        expected.sort_unstable();
        assert_eq!(
            sorted, expected,
            "case {case}: expected the log types {expected:?}, got {published:?}"
        );

        // Each log type is published for every destination type that receives a record, in the
        // order the model's enum declares them; the trace destination is published none.
        for log_type in &published {
            let reported: Vec<String> = templates
                .iter()
                .filter(|template| member(template, "logType") == *log_type)
                .map(|template| member(template, "deliveryDestinationType"))
                .collect();
            let expected: Vec<String> = DESTINATION_TYPE_ORDER
                .iter()
                .map(|destination_type| (*destination_type).to_string())
                .collect();
            assert_eq!(
                reported, expected,
                "case {case}|{log_type}: expected the destination types {expected:?}, got {reported:?}"
            );
        }

        // The trace destination is published no template of this sender's, whichever log types
        // it sends — traces included: a template states the valid and default values of a
        // RECORD delivery, and a trace delivery carries no record of fields.
        let traced = walk(
            &logs,
            &json!({
                "service": sender.service,
                "resourceTypes": [sender.resource_type],
                "deliveryDestinationTypes": [TRACE_DESTINATION_TYPE],
            }),
        )
        .await;
        assert!(
            traced.is_empty(),
            "case {case}: the trace destination is published no template, got {traced:?}"
        );

        // A log type this sender does not send has no template of its own, at any destination
        // type: neither under the sender's own filter nor under the trace destination's.
        for log_type in sender.does_not_send {
            for filter in [
                json!({
                    "service": sender.service,
                    "resourceTypes": [sender.resource_type],
                    "logTypes": [log_type],
                }),
                json!({
                    "service": sender.service,
                    "resourceTypes": [sender.resource_type],
                    "logTypes": [log_type],
                    "deliveryDestinationTypes": [TRACE_DESTINATION_TYPE],
                }),
            ] {
                let reported = walk(&logs, &filter).await;
                assert!(
                    reported.is_empty(),
                    "case {case}: this sender does not send {log_type:?}, so {filter} selects no template, got {reported:?}"
                );
            }
        }
    }
}

/// The destination that receives a STREAM of records publishes the record as the sender sent it;
/// the one that stores each record as its own item publishes the per-record renderings alone,
/// and the one that writes objects publishes the file layouts its sender publishes instead.
#[tokio::test]
async fn only_a_stream_destination_publishes_the_record_as_the_sender_sent_it() {
    let logs = Logs::new();

    // Senders of a fixed-shape line, of named fields of their own, of an event document and of
    // traces — every one of them a record a delivery can be written as the raw record of.
    let cases: [(&str, &str); 5] = [
        ("apigateway", "stages"),
        ("elasticloadbalancing", "loadbalancer"),
        ("eks", "cluster"),
        ("bedrock", "agent-alias"),
        ("bedrock-agentcore", "runtime"),
    ];

    for (service, resource_type) in cases {
        let templates = row(&logs, service, resource_type).await;
        assert!(
            !templates.is_empty(),
            "case {service}|{resource_type}: the catalogue publishes templates for this sender, got none"
        );
        for template in &templates {
            let case = key_of(template);
            let destination_type = member(template, "deliveryDestinationType");
            let formats = strings(template, "allowedOutputFormats")
                .unwrap_or_else(|| panic!("case {case}: a template carries allowedOutputFormats"));
            match destination_type.as_str() {
                STREAM_DESTINATION_TYPE => assert_eq!(
                    formats,
                    vec![
                        PLAIN_FORMAT.to_string(),
                        STRUCTURED_FORMAT.to_string(),
                        RAW_FORMAT.to_string()
                    ],
                    "case {case}: a stream destination publishes the record as the sender sent it, got {formats:?}"
                ),
                RECORD_DESTINATION_TYPE => assert_eq!(
                    formats,
                    vec![PLAIN_FORMAT.to_string(), STRUCTURED_FORMAT.to_string()],
                    "case {case}: a destination that stores each record publishes the per-record renderings alone, got {formats:?}"
                ),
                _ => assert!(
                    !formats.contains(&RAW_FORMAT.to_string()),
                    "case {case}: the raw record is published only where a delivery streams records, got {formats:?}"
                ),
            }
        }
    }

    // The same rule over the WHOLE catalogue, from the other side: wherever the raw record is
    // published the destination streams records, and the delimiters that apply to it are
    // published with it.
    let catalogue = walk(&logs, &json!({})).await;
    let mut streamed = 0usize;
    for template in &catalogue {
        let case = key_of(template);
        let formats = strings(template, "allowedOutputFormats")
            .unwrap_or_else(|| panic!("case {case}: a template carries allowedOutputFormats"));
        if !formats.contains(&RAW_FORMAT.to_string()) {
            continue;
        }
        streamed += 1;
        assert_eq!(
            member(template, "deliveryDestinationType"),
            STREAM_DESTINATION_TYPE,
            "case {case}: the raw record is published only where a delivery streams records, got {formats:?}"
        );
        assert!(
            strings(template, "allowedFieldDelimiters").is_some(),
            "case {case}: the raw record is a delimited rendering, so the delimiters are published with it, got {template}"
        );
    }
    assert!(
        streamed > 0,
        "the catalogue publishes the raw record somewhere, got no template that does"
    );
    // And no template anywhere is published for the trace destination.
    for template in &catalogue {
        let case = key_of(template);
        assert_ne!(
            member(template, "deliveryDestinationType"),
            TRACE_DESTINATION_TYPE,
            "case {case}: the trace destination is published no template"
        );
    }
}

/// What a sender publishes about ITS OWN record and object layout is reported as that sender
/// publishes it: the allowed fields and their mandatory flags, the fields a delivery carries when
/// it names none, the file layouts of its delivered objects, and the S3 Tables datasource it
/// binds to.
#[tokio::test]
async fn a_senders_own_record_and_object_layout_are_reported_as_it_publishes_them() {
    let logs = Logs::new();

    // The rows: a sender of its own named fields, the two kinds of resource one service sends
    // the same record from, and a fixed-shape line bound to an S3 Tables datasource.
    let cases: [Published; 4] = [
        // A sender whose record is its own: the instant of the event, the sending resource, the
        // severity and the message may not be dropped, while the record's own ingestion
        // timestamp and the identifier of the customization are allowed without being delivered.
        // It publishes no columnar layout for its delivered objects.
        (
            "a sender of its own named fields",
            "codewhisperer",
            "customization",
            "EVENT_LOGS",
            &[
                ("timestamp", false),
                ("event_timestamp", true),
                ("resource_id", true),
                ("customization_id", false),
                ("log_level", true),
                ("event_message", true),
            ],
            &[
                "event_timestamp",
                "resource_id",
                "log_level",
                "event_message",
            ],
            &[PLAIN_FORMAT, STRUCTURED_FORMAT, HEADED_FILE_FORMAT],
            None,
        ),
        // A sender that attributes its records with the resource ARN alone: it publishes no bare
        // resource id beside it, and every field of the line is one a delivery may drop.
        (
            "a sender attributing its records with the resource ARN alone",
            "events",
            "subscriber",
            "ERROR_LOGS",
            &[
                ("timestamp", false),
                ("resource_arn", false),
                ("message_timestamp_ms", false),
                ("subscriber_name", false),
                ("request_id", false),
                ("event_id", false),
                ("invocation_id", false),
                ("message_type", false),
                ("log_level", false),
                ("details", false),
                ("error", false),
            ],
            &[
                "resource_arn",
                "message_timestamp_ms",
                "subscriber_name",
                "request_id",
                "event_id",
                "invocation_id",
                "message_type",
                "log_level",
                "details",
                "error",
            ],
            &[
                PLAIN_FORMAT,
                STRUCTURED_FORMAT,
                HEADED_FILE_FORMAT,
                COLUMNAR_FORMAT,
            ],
            None,
        ),
        // The other kind of resource the same service sends those logs from publishes the same
        // record, so the rule is about the sender rather than one row of it.
        (
            "the same record from the other kind of resource that sends it",
            "events",
            "event-bus",
            "ERROR_LOGS",
            &[
                ("timestamp", false),
                ("resource_arn", false),
                ("message_timestamp_ms", false),
                ("subscriber_name", false),
                ("request_id", false),
                ("event_id", false),
                ("invocation_id", false),
                ("message_type", false),
                ("log_level", false),
                ("details", false),
                ("error", false),
            ],
            &[
                "resource_arn",
                "message_timestamp_ms",
                "subscriber_name",
                "request_id",
                "event_id",
                "invocation_id",
                "message_type",
                "log_level",
                "details",
                "error",
            ],
            &[
                PLAIN_FORMAT,
                STRUCTURED_FORMAT,
                HEADED_FILE_FORMAT,
                COLUMNAR_FORMAT,
            ],
            None,
        ),
        // A fixed-shape line that also binds to an S3 Tables datasource: every field of the
        // connection-log entry is mandatory, so a delivery of it carries them all.
        (
            "a fixed-shape line bound to an S3 Tables datasource",
            "elasticloadbalancing",
            "loadbalancer",
            "ALB_CONNECTION_LOGS",
            &[
                ("timestamp", true),
                ("client_ip", true),
                ("client_port", true),
                ("listener_port", true),
                ("tls_protocol", true),
                ("tls_cipher", true),
                ("tls_handshake_latency", true),
                ("leaf_client_cert_subject", true),
                ("leaf_client_cert_validity", true),
                ("leaf_client_cert_serial_number", true),
                ("tls_verify_status", true),
                ("choosen_cert_serial", true),
                ("choosen_cert_arn", true),
                ("conn_trace_id", true),
            ],
            &[
                "timestamp",
                "client_ip",
                "client_port",
                "listener_port",
                "tls_protocol",
                "tls_cipher",
                "tls_handshake_latency",
                "leaf_client_cert_subject",
                "leaf_client_cert_validity",
                "leaf_client_cert_serial_number",
                "tls_verify_status",
                "choosen_cert_serial",
                "choosen_cert_arn",
                "conn_trace_id",
            ],
            &[
                PLAIN_FORMAT,
                STRUCTURED_FORMAT,
                HEADED_FILE_FORMAT,
                COLUMNAR_FORMAT,
            ],
            Some(("aws_alb", "connection")),
        ),
    ];

    for (label, service, resource_type, log_type, fields, record_fields, object_formats, datasource) in
        cases
    {
        let templates = walk(
            &logs,
            &json!({
                "service": service,
                "resourceTypes": [resource_type],
                "logTypes": [log_type],
            }),
        )
        .await;
        assert!(
            !templates.is_empty(),
            "case {label}: the catalogue publishes templates for {service}|{log_type}|{resource_type}, got none"
        );

        let expected_fields: Vec<(String, bool)> = fields
            .iter()
            .map(|(name, mandatory)| ((*name).to_string(), *mandatory))
            .collect();
        let expected_record_fields: Vec<String> =
            record_fields.iter().map(|name| (*name).to_string()).collect();

        for template in &templates {
            let case = format!("{label} ({})", key_of(template));
            let reported = allowed_fields(template);
            assert_eq!(
                reported, expected_fields,
                "case {case}: expected the allowed fields {expected_fields:?}, got {reported:?}"
            );
            let reported = default_record_fields(template);
            assert_eq!(
                reported, expected_record_fields,
                "case {case}: expected the default record fields {expected_record_fields:?}, got {reported:?}"
            );
            // The datasource travels with the LOG TYPE, so it is reported at every destination
            // type that log type reaches, or nowhere.
            let reported = template.get("s3TablesIntegration");
            match datasource {
                Some((name, kind)) => {
                    let reported = reported.unwrap_or_else(|| {
                        panic!("case {case}: this log type binds to an S3 Tables datasource, got {template}")
                    });
                    assert_eq!(
                        (
                            reported.get("datasourceName").and_then(Value::as_str),
                            reported.get("datasourceType").and_then(Value::as_str)
                        ),
                        (Some(name), Some(kind)),
                        "case {case}: expected the datasource {name:?}/{kind:?}, got {reported}"
                    );
                }
                None => assert!(
                    reported.is_none(),
                    "case {case}: this log type binds to no S3 Tables datasource, got {reported:?}"
                ),
            }
            if member(template, "deliveryDestinationType") == OBJECT_DESTINATION_TYPE {
                let expected: Vec<String> = object_formats
                    .iter()
                    .map(|format| (*format).to_string())
                    .collect();
                let reported = strings(template, "allowedOutputFormats").unwrap_or_else(|| {
                    panic!("case {case}: a template carries allowedOutputFormats")
                });
                assert_eq!(
                    reported, expected,
                    "case {case}: expected the object renderings {expected:?}, got {reported:?}"
                );
            }
        }
    }
}

/// A sender's slice OPENS with the template that sender publishes first: the element order of
/// the catalogue is published data, so the first template of a filtered enumeration is the one
/// a client that takes the head of the list gets.
#[tokio::test]
async fn each_senders_slice_opens_with_the_template_it_publishes_first() {
    let logs = Logs::new();

    // (service, the key of the template its slice opens with)
    let cases: [(&str, &str); 8] = [
        ("bedrock", "bedrock|EVENT_LOGS|agent-alias|S3"),
        (
            "bedrock-agentcore",
            "bedrock-agentcore|APPLICATION_LOGS|workload-identity|S3",
        ),
        ("eks", "eks|AUTO_MODE_LOAD_BALANCING_LOGS|cluster|S3"),
        (
            "elasticloadbalancing",
            "elasticloadbalancing|ALB_CONNECTION_LOGS|loadbalancer|S3",
        ),
        ("autoloop", "autoloop|ERROR_LOGS|subscription|S3"),
        ("apigateway", "apigateway|EXECUTION_LOGS|stages|S3"),
        ("codewhisperer", "codewhisperer|EVENT_LOGS|customization|S3"),
        ("events", "events|ERROR_LOGS|subscriber|S3"),
    ];

    for (service, expected) in cases {
        let templates = walk(&logs, &json!({"service": service})).await;
        let reported = templates
            .first()
            .map(key_of)
            .unwrap_or_else(|| panic!("case {service}: the catalogue publishes this service, got no template"));
        assert_eq!(
            reported, expected,
            "case {service}: expected the slice to open with {expected:?}, got {reported:?}"
        );
        // The head of the unfiltered enumeration agrees with the head of the slice: one order,
        // whichever filter the request names.
        let unfiltered = walk(&logs, &json!({})).await;
        let first_of_service = unfiltered
            .iter()
            .find(|template| member(template, "service") == service)
            .map(key_of)
            .unwrap_or_else(|| panic!("case {service}: the whole catalogue holds this service"));
        assert_eq!(
            first_of_service, expected,
            "case {service}: expected the same first template in the whole catalogue, got {first_of_service:?}"
        );
    }
}

/// A delivery created from a published template takes THAT template's default record fields: the
/// catalogue is the contract a delivery is configured from, so what a sender publishes is what a
/// delivery of its logs reports.
#[tokio::test]
async fn a_delivery_takes_the_record_fields_its_senders_template_publishes() {
    let logs = Logs::new();
    let case = "a CodeWhisperer customization delivered to an S3 bucket";

    let published = walk(
        &logs,
        &json!({
            "service": "codewhisperer",
            "resourceTypes": ["customization"],
            "logTypes": ["EVENT_LOGS"],
            "deliveryDestinationTypes": [OBJECT_DESTINATION_TYPE],
        }),
    )
    .await;
    let template = published
        .first()
        .unwrap_or_else(|| panic!("case {case}: the catalogue publishes an object template, got none"));
    let expected = default_record_fields(template);

    logs.ok(
        PUT_SOURCE,
        &json!({
            "name": "customization-events",
            "resourceArn":
                "arn:aws:codewhisperer:eu-west-2:123456789012:customization/EXAMPLEcustomization",
            "logType": "EVENT_LOGS",
        }),
    )
    .await;
    let destination = logs
        .ok(
            PUT_DESTINATION,
            &json!({
                "name": "customization-objects",
                "deliveryDestinationConfiguration": {
                    "destinationResourceArn": "arn:aws:s3:::vended-logs-customization",
                },
            }),
        )
        .await;
    let destination_arn = destination["deliveryDestination"]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a put answers the destination's ARN, got {destination}"))
        .to_string();

    let created = logs
        .ok(
            CREATE_DELIVERY,
            &json!({
                "deliverySourceName": "customization-events",
                "deliveryDestinationArn": destination_arn,
            }),
        )
        .await;
    let id = created["delivery"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a created delivery carries an id, got {created}"))
        .to_string();
    let fetched = logs.ok(GET_DELIVERY, &json!({"id": id})).await;
    let reported = strings(&fetched["delivery"], "recordFields")
        .unwrap_or_else(|| panic!("case {case}: a delivery reports its record fields, got {fetched}"));
    assert_eq!(
        reported, expected,
        "case {case}: expected the published default record fields {expected:?}, got {reported:?}"
    );
}
