// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ROWS of the vended-log configuration-template catalogue
//! `DescribeConfigurationTemplates` publishes: which templates one log source contributes, the
//! order they are published in, and which of a record's fields a delivery carries when it names
//! none.
//!
//! The rules under test:
//!
//! * one log type contributes one template per delivery destination type ITS SENDER DELIVERS
//!   IT TO — a non-empty subset of the destination types that RECEIVE A RECORD — published in
//!   the order the model's `DeliveryDestinationType` enum DECLARES its values
//!   (`smithy_query(query="shape",shape="DeliveryDestinationType")` answers
//!   `[S3, CWL, FH, XRAY]`), so a slice opens with the template for the destination that writes
//!   objects wherever the sender writes objects at all; the X-Ray trace destination is published
//!   NO template, because a template states the valid and default values of a record delivery;
//! * `allowedFields` is "the allowed fields that a caller can use in the recordFields
//!   parameter" and `defaultDeliveryConfigValues.recordFields` is "the default record fields
//!   that will be delivered when a list of record fields is not provided", so the default list
//!   is those allowed fields IN ORDER less the ones that are allowed without being delivered,
//!   and a mandatory field — one that "must be present in the recordFields parameter" — is
//!   always among them;
//! * the catalogue holds one row per KIND OF RESOURCE a service sends logs from, with the log
//!   types that kind of resource sends;
//! * a `nextToken` is a POSITION in that published enumeration, so one naming a template the
//!   enumeration does not hold is refused rather than answered from somewhere else in it.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so what is
//! asserted here is what a client reads off the wire.
//!
//! The tables span what the rules generalise over rather than one slice: log sources whose
//! records are a generic vended-log envelope, an all-mandatory event document, an
//! OpenTelemetry log record, a fixed-shape load-balancer line and a service-specific family;
//! log types that reach three destination types and ones that reach four; and record schemas
//! that carry both fields a delivery leaves out by default, one of them as a MANDATORY field,
//! and neither of them.

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    encode_token, INVALID_NEXT_TOKEN_MESSAGE,
};

use crate::harness::Logs;

/// The operation under test.
const DESCRIBE: &str = "DescribeConfigurationTemplates";

/// The refusal this operation models for a request it cannot answer, and the cursor kind its
/// tokens carry.
const VALIDATION: &str = "ValidationException";
const CURSOR_KIND: &str = "DescribeConfigurationTemplates";

/// The largest page the model admits (`DescribeLimit` is `range 1..50`), so one filtered
/// enumeration below is read in a single call.
const MAX_LIMIT: usize = 50;

/// The `DeliveryDestinationType`s the model's enum names, in the order it DECLARES them
/// (`smithy_query(query="shape",shape="DeliveryDestinationType")`), the one of them that
/// receives traces — which the catalogue publishes no template for — and the `logType` of a
/// source that sends traces as the `logType` member's own catalogue spells it
/// (`smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`).
const TRACE_DESTINATION_TYPE: &str = "XRAY";
const TRACE_LOG_TYPE: &str = "TRACES";

/// The destination types that receive log RECORDS: every one the model names but the trace
/// destination, in the order the enum declares them — the destination types the catalogue
/// publishes a template for.
const RECORD_DESTINATION_TYPES: [&str; 3] = ["S3", "CWL", "FH"];

/// The destination types a log type delivered to CloudWatch Logs ALONE is published for: the
/// log group that stores each delivered record, and neither of the two its sender does not
/// write to.
const LOG_GROUP_DELIVERY_ONLY: [&str; 1] = ["CWL"];

/// The fields a template ALLOWS that a delivery created without a record field list does not
/// carry: the record's own ingestion timestamp and the bare id of the sending resource that the
/// resource ARN beside it already names. Stated here rather than imported, so an edit to the
/// published catalogue is caught instead of agreeing with itself.
const FIELDS_NOT_DELIVERED_BY_DEFAULT: [&str; 2] = ["timestamp", "resource_id"];

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

/// The destination types a list of templates was published for, in order.
fn destination_types(templates: &[Value]) -> Vec<String> {
    templates
        .iter()
        .map(|template| member(template, "deliveryDestinationType"))
        .collect()
}

/// One log type contributes one template per destination type that receives a record, in the
/// order the model's enum declares them — and over the whole catalogue the trace destination is
/// published no template.
#[tokio::test]
async fn a_log_types_templates_are_published_in_the_models_destination_type_order() {
    let logs = Logs::new();

    // (service, log type, resource type, the destination types it is published for)
    let cases: [(&str, &str, &str, &[&str]); 10] = [
        (
            "apigateway",
            "EXECUTION_LOGS",
            "stages",
            &RECORD_DESTINATION_TYPES,
        ),
        (
            "workmail",
            "ACCESS_CONTROL_LOGS",
            "organization",
            &RECORD_DESTINATION_TYPES,
        ),
        (
            "elasticloadbalancing",
            "ALB_CONNECTION_LOGS",
            "loadbalancer",
            &RECORD_DESTINATION_TYPES,
        ),
        (
            "eks",
            "AUTO_MODE_LOAD_BALANCING_LOGS",
            "cluster",
            &RECORD_DESTINATION_TYPES,
        ),
        (
            "bedrock",
            "EVENT_LOGS",
            "agent-alias",
            &RECORD_DESTINATION_TYPES,
        ),
        ("mediatailor", "TRANSCODE_LOGS", "playbackConfiguration", &RECORD_DESTINATION_TYPES),
        // A source that sends traces: its records are published for the same destinations, and
        // the trace destination is published no template for them either.
        (
            "bedrock-agentcore",
            TRACE_LOG_TYPE,
            "runtime",
            &RECORD_DESTINATION_TYPES,
        ),
        (
            "bedrock",
            TRACE_LOG_TYPE,
            "knowledge-base",
            &RECORD_DESTINATION_TYPES,
        ),
        // A sender that delivers its log type to a LOG GROUP alone: one template, and no object
        // or stream template beside it.
        (
            "securityhub",
            "SECURITY_FINDING_LOGS",
            "hubv2",
            &LOG_GROUP_DELIVERY_ONLY,
        ),
        (
            "securityhub",
            "SECURITY_FINDING_LOGS",
            "hub",
            &LOG_GROUP_DELIVERY_ONLY,
        ),
    ];

    for (service, log_type, resource_type, expected) in cases {
        let case = format!("{service}|{log_type}|{resource_type}");
        let templates = slice(&logs, service, log_type, resource_type).await;
        let reported = destination_types(&templates);
        let expected: Vec<String> = expected.iter().map(|value| (*value).to_string()).collect();
        assert_eq!(
            reported, expected,
            "case {case}: expected the destination types {expected:?}, got {reported:?}"
        );
    }

    // The same rule over the WHOLE catalogue: the trace destination receives traces alone, and
    // every log type reaches each destination that receives records.
    let catalogue = walk(&logs, &json!({})).await;
    assert!(
        catalogue.len() > MAX_LIMIT,
        "case the whole catalogue: it is read over several pages, got {} templates",
        catalogue.len()
    );
    for template in &catalogue {
        let case = format!(
            "{}|{}|{}",
            member(template, "service"),
            member(template, "logType"),
            member(template, "resourceType")
        );
        let destination_type = member(template, "deliveryDestinationType");
        assert!(
            RECORD_DESTINATION_TYPES.contains(&destination_type.as_str()),
            "case {case}: a template names a destination type that receives a record, got {destination_type:?}"
        );
        assert_ne!(
            destination_type, TRACE_DESTINATION_TYPE,
            "case {case}: the trace destination is published no template"
        );
    }
    // Each destination that receives records serves the log types whose sender delivers them
    // there, and one log type's templates are published in the enum's order — asserted over the
    // WHOLE catalogue, group by group, so a row that publishes a set of its own is held to the
    // same ordering rule as the rest.
    let group_of = |template: &Value| -> String {
        format!(
            "{}|{}|{}",
            member(template, "service"),
            member(template, "logType"),
            member(template, "resourceType")
        )
    };
    let mut groups: Vec<String> = Vec::new();
    for template in &catalogue {
        let group = group_of(template);
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    for group in &groups {
        let published: Vec<String> = catalogue
            .iter()
            .filter(|template| &group_of(template) == group)
            .map(|template| member(template, "deliveryDestinationType"))
            .collect();
        let in_enum_order: Vec<String> = RECORD_DESTINATION_TYPES
            .iter()
            .filter(|destination_type| published.iter().any(|named| named == *destination_type))
            .map(|destination_type| (*destination_type).to_string())
            .collect();
        assert!(
            !published.is_empty(),
            "case {group}: a published log type reaches at least one destination type"
        );
        assert_eq!(
            published, in_enum_order,
            "case {group}: expected the destination types it reaches in the enum's order, got \
             {published:?}"
        );
    }
}

/// The default record field list names the allowed fields a delivery carries when it names
/// none: those fields in order, less the ones that are allowed without being delivered, and
/// never less a mandatory one.
#[tokio::test]
async fn the_default_record_fields_are_the_allowed_fields_a_delivery_carries_by_default() {
    let logs = Logs::new();

    // (service, log type, resource type, the default record field list the catalogue publishes)
    let cases: [(&str, &str, &str, &[&str]); 8] = [
        // A generic vended-log envelope: the record's own timestamp and the sending resource's
        // bare id are allowed without being delivered by default.
        (
            "apigateway",
            "EXECUTION_LOGS",
            "stages",
            &[
                "resource_arn",
                "event_timestamp",
                "api_id",
                "stage",
                "resource_path",
                "http_method",
                "payload",
            ],
        ),
        // A sender that publishes no variable naming the sending resource among its suffix path
        // fields publishes no bare `resource_id` field either, so its default list is its whole
        // record less the record's own ingestion timestamp.
        (
            "autoloop",
            "ERROR_LOGS",
            "subscription",
            &[
                "resource_arn",
                "event_timestamp",
                "log_timestamp",
                "transformer_arn",
                "object_identifier",
                "event_identifier",
                "trace_identifier",
                "log_message",
            ],
        ),
        (
            "eks",
            "AUTO_MODE_LOAD_BALANCING_LOGS",
            "cluster",
            &[
                "resource_arn",
                "event_timestamp",
                "level",
                "stream",
                "message",
                "err",
                "error",
                "controller",
                "controllerGroup",
                "controllerKind",
                "reconcileID",
                "worker-count",
                "attempt",
                "delay",
                "totalItems",
            ],
        ),
        (
            "events",
            "ERROR_LOGS",
            "subscriber",
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
        ),
        (
            "workmail",
            "ACCESS_CONTROL_LOGS",
            "organization",
            &[
                "organization_arn",
                "event_timestamp",
                "request_id",
                "user_id",
                "impersonator_id",
                "protocol",
                "source_ip",
                "user_agent",
                "scope",
                "rule_id",
                "access_granted",
            ],
        ),
        // One event document whose every field is mandatory: its own `timestamp` MUST be
        // present in the record fields, so it is delivered by default.
        (
            "bedrock",
            "EVENT_LOGS",
            "agent-alias",
            &[
                "timestamp",
                "resourceId",
                "traceId",
                "spanId",
                "sessionId",
                "requestId",
                "operation",
                "attributes",
                "body",
                "eventType",
                "eventVersion",
                "eventName",
                "level",
            ],
        ),
        // An AgentCore record: the vended-log envelope the resource attributes it with,
        // followed by the OpenTelemetry log record it envelopes. The record's own `timestamp`
        // and the bare `resource_id` are allowed without being delivered by default.
        (
            "bedrock-agentcore",
            "APPLICATION_LOGS",
            "workload-identity",
            &[
                "resource_arn",
                "event_timestamp",
                "account_id",
                "request_id",
                "session_id",
                "span_id",
                "trace_id",
                "service_name",
                "operation",
                "request_payload",
                "response_payload",
                "resource",
                "attributes",
                "timeUnixNano",
                "severityNumber",
                "severityText",
                "body",
                "traceId",
                "spanId",
            ],
        ),
        // A record that carries the sending service's own event document: its own ingestion
        // `timestamp` is allowed without being delivered by default, and every other field it
        // publishes is delivered.
        (
            "mediatailor",
            "AD_DECISION_SERVER_LOGS",
            "playbackConfiguration",
            &[
                "originId",
                "eventTimestamp",
                "awsAccountId",
                "customerId",
                "eventType",
                "sessionType",
                "requestId",
                "sessionId",
                "playbackConfigurationName",
                "vastResponse",
                "vastAds",
                "adBreaks",
                "beaconInfo",
                "trackingEvents",
            ],
        ),
    ];

    for (service, log_type, resource_type, expected) in cases {
        let templates = slice(&logs, service, log_type, resource_type).await;
        assert!(
            !templates.is_empty(),
            "case {service}|{log_type}|{resource_type}: the catalogue publishes templates for this log source, got none"
        );
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        for template in &templates {
            let case = format!(
                "{service}|{log_type}|{resource_type}|{}",
                member(template, "deliveryDestinationType")
            );
            let allowed = template
                .get("allowedFields")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("case {case}: a template carries allowedFields, got {template}"))
                .clone();
            let reported: Vec<String> = template
                .get("defaultDeliveryConfigValues")
                .and_then(|defaults| defaults.get("recordFields"))
                .and_then(Value::as_array)
                .unwrap_or_else(|| {
                    panic!("case {case}: a template carries default recordFields, got {template}")
                })
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case}: recordFields holds strings, got {value}"))
                        .to_string()
                })
                .collect();
            assert_eq!(
                reported, expected,
                "case {case}: expected the default record fields {expected:?}, got {reported:?}"
            );

            // The invariants the published list must satisfy whatever the sender: it is the
            // allowed fields in order, it names no field the template does not allow, every
            // mandatory field is in it, and a field that is allowed without being delivered is
            // in it only when a delivery may not drop it.
            let names: Vec<String> = allowed
                .iter()
                .map(|field| member(field, "name"))
                .collect();
            let mut remaining = names.iter();
            assert!(
                reported
                    .iter()
                    .all(|name| remaining.any(|allowed| allowed == name)),
                "case {case}: the default record fields are allowed fields in the allowed order {names:?}, got {reported:?}"
            );
            for field in &allowed {
                let name = member(field, "name");
                let mandatory = field.get("mandatory").and_then(Value::as_bool) == Some(true);
                if mandatory {
                    assert!(
                        reported.contains(&name),
                        "case {case}: a mandatory field is delivered by default, got {reported:?} without {name:?}"
                    );
                } else if FIELDS_NOT_DELIVERED_BY_DEFAULT.contains(&name.as_str()) {
                    assert!(
                        !reported.contains(&name),
                        "case {case}: {name:?} is allowed without being delivered by default, got {reported:?}"
                    );
                }
            }
            for name in &reported {
                assert!(
                    names.contains(name),
                    "case {case}: a delivered field is one the template allows, got {name:?} outside {names:?}"
                );
            }
        }
    }
}

/// A token is a POSITION in the published enumeration: one naming a template that enumeration
/// does not hold is refused rather than answered from somewhere else in it.
#[tokio::test]
async fn a_token_naming_no_template_of_the_enumeration_is_refused() {
    let logs = Logs::new();

    // Each row is a cursor of this operation's own kind, carrying no filter — so it continues
    // the unfiltered enumeration — whose `after` key names no template the catalogue holds:
    // a key of the published form that no row produces, one that sorts inside the published
    // order (so a resume that searched for a position rather than the template itself would
    // answer from the middle of the list), and an empty one.
    let cases: [(&str, &str); 4] = [
        ("a key of the published form that no row carries", "nope|NOPE_LOGS|nope|S3"),
        (
            "a key naming a service the catalogue carries with a log type it does not",
            "workmail|NOPE_LOGS|organization|S3",
        ),
        (
            "a key that sorts between two published ones",
            "eks|AUTO_MODE_COMPUTE_LOGS|cluster|AAA",
        ),
        ("an empty key", ""),
    ];

    for (case, after) in cases {
        let token = encode_token(&json!({"kind": CURSOR_KIND, "after": after}));
        let (code, message) = logs
            .refusal(DESCRIBE, &json!({"nextToken": token, "limit": MAX_LIMIT}))
            .await;
        assert_eq!(
            code, VALIDATION,
            "case {case:?}: expected {VALIDATION}, got {code} {message:?}"
        );
        assert_eq!(
            message, INVALID_NEXT_TOKEN_MESSAGE,
            "case {case:?}: expected the message {INVALID_NEXT_TOKEN_MESSAGE:?}, got {message:?}"
        );
    }

    // A token the operation itself minted still resumes: the refusal is about the position, not
    // about continuing at all.
    let first = logs.ok(DESCRIBE, &json!({"limit": 1})).await;
    let token = first
        .get("nextToken")
        .and_then(Value::as_str)
        .expect("a first page of one template out of many carries a nextToken");
    let next = logs
        .ok(DESCRIBE, &json!({"limit": 1, "nextToken": token}))
        .await;
    let page = next
        .get("configurationTemplates")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the continuation carries configurationTemplates, got {next}"));
    assert_eq!(
        page.len(),
        1,
        "case a token this operation minted: the next page holds one template, got {page:?}"
    );
}

/// The catalogue holds one row per KIND OF RESOURCE a service sends logs from, each publishing
/// the log types that kind of resource sends.
#[tokio::test]
async fn the_catalogue_holds_one_row_per_kind_of_resource_a_service_sends_logs_from() {
    let logs = Logs::new();

    // (service, the kinds of resource it sends logs from, the log types each of them sends)
    //
    // Source: the `logType` member's own catalogue
    // (`smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`), which
    // names the senders one kind at a time — "For Amazon Bedrock AgentCore Gateway, the valid
    // values are APPLICATION_LOGS and TRACES. [...] For Amazon Bedrock AgentCore Runtime, the
    // valid values are APPLICATION_LOGS, USAGE_LOGS, and TRACES."
    /// One kind of resource a service sends logs from, and the log types it sends.
    type ResourceRow = (&'static str, &'static [&'static str]);
    let cases: [(&str, &[ResourceRow]); 3] = [
        (
            "bedrock-agentcore",
            &[
                ("gateway", &["APPLICATION_LOGS", TRACE_LOG_TYPE]),
                ("memory", &["APPLICATION_LOGS", TRACE_LOG_TYPE]),
                ("payments", &["APPLICATION_LOGS", TRACE_LOG_TYPE]),
                (
                    "runtime",
                    &["APPLICATION_LOGS", "USAGE_LOGS", TRACE_LOG_TYPE],
                ),
                ("tools", &["APPLICATION_LOGS", "USAGE_LOGS", TRACE_LOG_TYPE]),
                ("workload-identity", &["APPLICATION_LOGS", TRACE_LOG_TYPE]),
            ],
        ),
        (
            "bedrock",
            &[
                // "For Amazon Bedrock Agents, the valid values are APPLICATION_LOGS and
                // EVENT_LOGS" — this sender sends no traces, so no trace template is published
                // for it; its knowledge base sends traces instead of event logs.
                ("agent-alias", &["EVENT_LOGS", "APPLICATION_LOGS"]),
                ("knowledge-base", &["APPLICATION_LOGS", TRACE_LOG_TYPE]),
            ],
        ),
        (
            "events",
            &[
                ("subscriber", &["ERROR_LOGS", "INFO_LOGS"]),
                ("event-bus", &["ERROR_LOGS", "INFO_LOGS"]),
            ],
        ),
    ];

    for (service, rows) in cases {
        let reported = walk(&logs, &json!({"service": service})).await;
        for template in &reported {
            assert_eq!(
                member(template, "service"),
                service,
                "case {service}: every selected template is this service's, got {template}"
            );
        }

        // Every (kind of resource, log type) pair the service publishes, and nothing else.
        let mut expected: Vec<String> = Vec::new();
        let mut published = 0usize;
        for (resource_type, log_types) in rows {
            for log_type in *log_types {
                expected.push(format!("{resource_type}|{log_type}"));
                published += RECORD_DESTINATION_TYPES.len();
            }
        }
        expected.sort_unstable();
        let mut pairs: Vec<String> = reported
            .iter()
            .map(|template| {
                format!(
                    "{}|{}",
                    member(template, "resourceType"),
                    member(template, "logType")
                )
            })
            .collect();
        pairs.sort_unstable();
        pairs.dedup();
        assert_eq!(
            pairs, expected,
            "case {service}: expected the published rows {expected:?}, got {pairs:?}"
        );
        assert_eq!(
            reported.len(),
            published,
            "case {service}: expected {published} templates, one per row per destination type it reaches, got {}",
            reported.len()
        );
    }
}
