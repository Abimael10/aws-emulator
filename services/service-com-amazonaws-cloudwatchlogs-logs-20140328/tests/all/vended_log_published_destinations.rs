// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH delivery destinations the vended-log configuration-template
//! catalogue publishes a template for.
//!
//! RULE: a configuration template carries "the valid and default values that are used when
//! creating delivery sources, delivery destinations, and deliveries" for ONE log type delivered
//! to ONE kind of destination, so the catalogue publishes a template only for the destination
//! types the SENDING SERVICE delivers that log type to. A caller enumerating where it may
//! deliver a log type is therefore offered no destination its sender never writes to, and no
//! packaging contract — an object layout, the variables an S3 suffix path may use — for a
//! destination the sender writes no objects to.
//!
//! The table spans what the rule generalises over: a sender that delivers its log type to a LOG
//! GROUP alone (Security Hub findings, from each of the two hub resources that send them), and
//! senders that deliver theirs to all three kinds of destination that receive a record — asserted
//! per destination type the model names, the trace destination included, so a template published
//! for a destination the sender does not write to fails here whichever destination it is.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.DescribeConfigurationTemplates` through the router that mounts
//! it — so what is asserted is what a client reads off the wire.

use serde_json::{json, Value};

use crate::harness::Logs;

/// The operation under test.
const DESCRIBE: &str = "DescribeConfigurationTemplates";

/// The `DeliveryDestinationType`s the model's enum names
/// (`smithy_query(query="shape",shape="DeliveryDestinationType")`).
const DESTINATION_TYPES: [&str; 4] = ["S3", "CWL", "FH", "XRAY"];

/// The destination type that stores each delivered RECORD as its own item: a log group of this
/// service.
const LOG_GROUP_DESTINATION_TYPE: &str = "CWL";

/// The destination type a delivery writes OBJECTS to, which is the one an object layout and a
/// suffix-path contract are published for.
const OBJECT_DESTINATION_TYPE: &str = "S3";

/// One sender under test: the catalogue row it is published as, and the destination types it
/// delivers its log type to.
struct Sender {
    service: &'static str,
    log_type: &'static str,
    resource_type: &'static str,
    /// The destination types the catalogue must publish a template of this log type for.
    delivered_to: &'static [&'static str],
}

/// The destination types a sender that delivers to a log group ALONE is published for, and the
/// ones a sender that delivers to every destination that receives a record is published for, in
/// the order the model's enum declares them.
const LOG_GROUP_ONLY: [&str; 1] = [LOG_GROUP_DESTINATION_TYPE];
const EVERY_RECORD_DESTINATION: [&str; 3] = ["S3", "CWL", "FH"];

/// The senders the rule is asserted over: the two hub resources Security Hub vends findings
/// from, and two senders whose logs reach every destination that receives a record.
const SENDERS: &[Sender] = &[
    Sender {
        service: "securityhub",
        log_type: "SECURITY_FINDING_LOGS",
        resource_type: "hubv2",
        delivered_to: &LOG_GROUP_ONLY,
    },
    Sender {
        service: "securityhub",
        log_type: "SECURITY_FINDING_LOGS",
        resource_type: "hub",
        delivered_to: &LOG_GROUP_ONLY,
    },
    Sender {
        service: "workmail",
        log_type: "ACCESS_CONTROL_LOGS",
        resource_type: "organization",
        delivered_to: &EVERY_RECORD_DESTINATION,
    },
    Sender {
        service: "apigateway",
        log_type: "EXECUTION_LOGS",
        resource_type: "stages",
        delivered_to: &EVERY_RECORD_DESTINATION,
    },
];

/// The templates one filter selects, as the operation reports them.
async fn published(logs: &Logs, filter: &Value) -> Vec<Value> {
    let answer = logs.ok(DESCRIBE, filter).await;
    answer["configurationTemplates"]
        .as_array()
        .unwrap_or_else(|| panic!("case {filter}: a templates page, got {answer}"))
        .clone()
}

/// A string member of a reported template.
fn member(template: &Value, name: &str) -> String {
    template
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a reported template carries {name}, got {template}"))
        .to_string()
}

/// RULE: a template stands exactly for the destination types its sender delivers the log type
/// to, and for no others.
#[tokio::test]
async fn a_template_stands_for_the_destination_types_its_sender_delivers_to() {
    let logs = Logs::new();
    for sender in SENDERS {
        let slice = format!(
            "{}|{}|{}",
            sender.service, sender.log_type, sender.resource_type
        );
        // Every destination type the model names, asked for one at a time: the ones the sender
        // delivers to carry exactly one template, and the rest carry none at all.
        for destination_type in DESTINATION_TYPES {
            let case = format!("{slice}|{destination_type}");
            let reported = published(
                &logs,
                &json!({
                    "service": sender.service,
                    "logTypes": [sender.log_type],
                    "resourceTypes": [sender.resource_type],
                    "deliveryDestinationTypes": [destination_type],
                }),
            )
            .await;
            let expected = usize::from(sender.delivered_to.contains(&destination_type));
            assert_eq!(
                reported.len(),
                expected,
                "case {case}: expected {expected} template(s) for this destination type, got \
                 {reported:?}"
            );
            for template in &reported {
                assert_eq!(
                    member(template, "deliveryDestinationType"),
                    destination_type,
                    "case {case}: a selected template is for that destination type, got \
                     {template}"
                );
            }
        }

        // The unfiltered slice is those same destination types, in the order the model's enum
        // declares them.
        let reported: Vec<String> = published(
            &logs,
            &json!({
                "service": sender.service,
                "logTypes": [sender.log_type],
                "resourceTypes": [sender.resource_type],
            }),
        )
        .await
        .iter()
        .map(|template| member(template, "deliveryDestinationType"))
        .collect();
        let expected: Vec<String> = sender
            .delivered_to
            .iter()
            .map(|destination_type| (*destination_type).to_string())
            .collect();
        assert_eq!(
            reported, expected,
            "case {slice}: expected the destination types {expected:?}, got {reported:?}"
        );
    }
}

/// RULE (what follows from it): a template published for a destination that stores records
/// carries no object-layout contract, because there are no objects to lay out — so a sender
/// that delivers to a log group alone publishes neither an S3 delivery configuration nor the
/// variables an S3 suffix path may use, anywhere in its slice; a sender that does write objects
/// publishes both, on its object template.
#[tokio::test]
async fn an_object_layout_is_published_only_where_a_delivery_writes_objects() {
    let logs = Logs::new();
    for sender in SENDERS {
        let slice = format!(
            "{}|{}|{}",
            sender.service, sender.log_type, sender.resource_type
        );
        for template in published(
            &logs,
            &json!({
                "service": sender.service,
                "logTypes": [sender.log_type],
                "resourceTypes": [sender.resource_type],
            }),
        )
        .await
        {
            let destination_type = member(&template, "deliveryDestinationType");
            let case = format!("{slice}|{destination_type}");
            let writes_objects = destination_type == OBJECT_DESTINATION_TYPE;
            assert_eq!(
                template
                    .get("defaultDeliveryConfigValues")
                    .and_then(|defaults| defaults.get("s3DeliveryConfiguration"))
                    .is_some(),
                writes_objects,
                "case {case}: an object layout is published exactly where the delivery writes \
                 objects, got {template}"
            );
            assert_eq!(
                template.get("allowedSuffixPathFields").is_some(),
                writes_objects,
                "case {case}: the suffix-path variables are published exactly where the delivery \
                 writes objects, got {template}"
            );
        }
    }
}

/// The content of the ONE template a sender that delivers to a log group alone publishes: its
/// record is the sending service's own document, so the one field that carries it is mandatory,
/// the document itself is the one rendering on offer, and a delivery that names no record field
/// list carries that field.
#[tokio::test]
async fn the_log_group_template_of_a_document_record_publishes_that_document() {
    let logs = Logs::new();
    // The field that carries a Security Hub finding, and the rendering a record that IS one
    // document is on offer in: the delivered line.
    const DOCUMENT_FIELD: &str = "event";
    const DOCUMENT_FORMAT: &str = "plain";
    for resource_type in ["hubv2", "hub"] {
        let case = format!("securityhub|SECURITY_FINDING_LOGS|{resource_type}");
        let reported = published(
            &logs,
            &json!({
                "service": "securityhub",
                "logTypes": ["SECURITY_FINDING_LOGS"],
                "resourceTypes": [resource_type],
                "deliveryDestinationTypes": [LOG_GROUP_DESTINATION_TYPE],
            }),
        )
        .await;
        assert_eq!(
            reported.len(),
            1,
            "case {case}: one template stands for the log-group destination, got {reported:?}"
        );
        let template = &reported[0];
        assert_eq!(
            template.get("allowedFields"),
            Some(&json!([{"name": DOCUMENT_FIELD, "mandatory": true}])),
            "case {case}: the record is one document, carried by one mandatory field, got \
             {template}"
        );
        assert_eq!(
            template.get("allowedOutputFormats"),
            Some(&json!([DOCUMENT_FORMAT])),
            "case {case}: a record that is one document is on offer as that document alone, got \
             {template}"
        );
        let defaults = template
            .get("defaultDeliveryConfigValues")
            .unwrap_or_else(|| panic!("case {case}: a template publishes its defaults, got {template}"));
        assert_eq!(
            defaults.get("recordFields"),
            Some(&json!([DOCUMENT_FIELD])),
            "case {case}: a delivery that names no record fields carries the document, got \
             {template}"
        );
        assert_eq!(
            member(template, "allowedActionForAllowVendedLogsDeliveryForResource"),
            "securityhub:AllowVendedLogDeliveryForResource",
            "case {case}: the template names the IAM action of the sending resource's service, \
             got {template}"
        );
    }
}
