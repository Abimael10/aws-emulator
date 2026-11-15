// SPDX-License-Identifier: CC0-1.0

//! The `MessageAttributes` map a `lambda` subscription's event record carries.
//!
//! The RULE asserted here: a function's event record reports each published message
//! attribute as `{"Type": …, "Value": …}` where `Type` names the FORM the published value
//! arrived in rather than the `DataType` the API declared for it — `Binary` for an attribute
//! published as bytes (`MessageAttributeValue$BinaryValue`), with the base64 of those bytes
//! as its value, and `String` for one published as text
//! (`MessageAttributeValue$StringValue`), with that text, whatever label its `DataType`
//! carried. So a `Number` attribute reaches a function as a `String` carrying its decimal
//! text, and so does a custom-labelled one; a message published with no attributes at all
//! delivers an empty map, which the member always carries.
//!
//! That is the record's own shape and it differs from the JSON envelope a queue or a
//! subscribed URL receives, which carries the declared `DataType` verbatim — the member a
//! handler switching on `Sns.MessageAttributes[…]["Type"]` reads is this one.
//!
//! The rows differ along what the rule generalises over: which `DataType` the publish
//! declared (`String`, `Number`, `Binary`), whether that label is a custom one
//! (`String.Array`, `Number.float` — "You can also append custom labels",
//! `smithy_query(query="doc", shape="MessageAttributeValue", member="DataType")`), how many
//! attributes one message carries, and a message carrying none.
//!
//! The `Subject` member belongs to the same shape and is asserted beside them: a record
//! carries it whether or not the message has one, as `null` for a publish that set none.

use serde_json::{json, Value};

use crate::lambda_subscription_delivery::{
    emulator, given_function, grant, invoked, publish, sns, subscribe, INVOKE_ACTION,
    LAMBDA_PROTOCOL, SNS_PRINCIPAL,
};
use crate::sqs_subscription_delivery::element;

/// One published attribute: its name, the `DataType` the publish declares, and the value it
/// carries — as text, or as the base64 of the bytes a `BinaryValue` is published as.
struct Published {
    name: &'static str,
    data_type: &'static str,
    /// The value and which member carries it: `true` for a `BinaryValue`.
    value: &'static str,
    binary: bool,
}

/// One row: the attributes a publish carries and the whole map the record must then report.
struct Row {
    case: &'static str,
    published: Vec<Published>,
    /// The `Subject` the publish carries, if any — which the record reports as `null` when
    /// there is none.
    subject: Option<&'static str>,
    /// The `Sns.MessageAttributes` object the record must carry, exactly.
    expected: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a message carrying no attributes at all",
            published: Vec::new(),
            subject: None,
            expected: json!({}),
        },
        Row {
            case: "a message carrying a subject and no attributes",
            published: Vec::new(),
            subject: Some("an order was placed"),
            expected: json!({}),
        },
        Row {
            case: "a String attribute",
            published: vec![Published {
                name: "colour",
                data_type: "String",
                value: "blue",
                binary: false,
            }],
            subject: None,
            expected: json!({"colour": {"Type": "String", "Value": "blue"}}),
        },
        Row {
            case: "a Number attribute",
            published: vec![Published {
                name: "count",
                data_type: "Number",
                value: "42",
                binary: false,
            }],
            subject: None,
            expected: json!({"count": {"Type": "String", "Value": "42"}}),
        },
        Row {
            case: "a Binary attribute",
            published: vec![Published {
                name: "blob",
                data_type: "Binary",
                value: "AAFzaW0ycmVhbP8=",
                binary: true,
            }],
            subject: None,
            expected: json!({"blob": {"Type": "Binary", "Value": "AAFzaW0ycmVhbP8="}}),
        },
        Row {
            case: "a custom-labelled String attribute",
            published: vec![Published {
                name: "names",
                data_type: "String.Array",
                value: "[\"first\",\"second\"]",
                binary: false,
            }],
            subject: None,
            expected: json!({"names": {"Type": "String", "Value": "[\"first\",\"second\"]"}}),
        },
        Row {
            case: "a custom-labelled Number attribute",
            published: vec![Published {
                name: "ratio",
                data_type: "Number.float",
                value: "1.5",
                binary: false,
            }],
            subject: None,
            expected: json!({"ratio": {"Type": "String", "Value": "1.5"}}),
        },
        Row {
            case: "one message carrying every form at once, with a subject",
            published: vec![
                Published {
                    name: "colour",
                    data_type: "String",
                    value: "blue",
                    binary: false,
                },
                Published {
                    name: "count",
                    data_type: "Number",
                    value: "42",
                    binary: false,
                },
                Published {
                    name: "blob",
                    data_type: "Binary",
                    value: "AAFzaW0ycmVhbP8=",
                    binary: true,
                },
            ],
            subject: Some("every form"),
            expected: json!({
                "colour": {"Type": "String", "Value": "blue"},
                "count": {"Type": "String", "Value": "42"},
                "blob": {"Type": "Binary", "Value": "AAFzaW0ycmVhbP8="},
            }),
        },
    ]
}

/// The `Publish` form parameters that carry `published` and `subject`, beside the message
/// itself.
fn publish_params(published: &[Published], subject: Option<&str>) -> Vec<(String, String)> {
    let mut params = vec![("Message".to_string(), "the attributed body".to_string())];
    if let Some(subject) = subject {
        params.push(("Subject".to_string(), subject.to_string()));
    }
    for (index, attribute) in published.iter().enumerate() {
        let entry = index + 1;
        params.push((
            format!("MessageAttributes.entry.{entry}.Name"),
            attribute.name.to_string(),
        ));
        params.push((
            format!("MessageAttributes.entry.{entry}.Value.DataType"),
            attribute.data_type.to_string(),
        ));
        let member = if attribute.binary {
            "BinaryValue"
        } else {
            "StringValue"
        };
        params.push((
            format!("MessageAttributes.entry.{entry}.Value.{member}"),
            attribute.value.to_string(),
        ));
    }
    params
}

#[tokio::test]
async fn a_function_records_attributes_by_the_form_their_value_arrived_in() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let topic = element(
            &sns(
                &emulator,
                "CreateTopic",
                &[("Name", &format!("attributes-{index}"))],
            )
            .await,
            "TopicArn",
        );
        let function = format!("attribute-probe-{index}");
        let function_arn = given_function(&emulator, &function).await;
        grant(
            &emulator,
            &function,
            "sns-invoke",
            INVOKE_ACTION,
            SNS_PRINCIPAL,
            Some(&topic),
        )
        .await;
        subscribe(&emulator, &topic, LAMBDA_PROTOCOL, &function_arn).await;

        let owned = publish_params(&row.published, row.subject);
        let params: Vec<(&str, &str)> = owned
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        publish(&emulator, &topic, &params).await;

        let events = invoked(&emulator, &function, 1).await;
        assert_eq!(
            events.len(),
            1,
            "case {case:?}: expected exactly one invocation, got {events:?}"
        );
        let record = &events[0]["Records"][0]["Sns"];
        let carried = &record["MessageAttributes"];
        assert_eq!(
            carried, &row.expected,
            "case {case:?}: expected the record's attribute map {}, got {carried}",
            row.expected
        );
        // The subject member is there whichever way the publish went: the text it carried,
        // and `null` for a publish that set none.
        let subject = record
            .as_object()
            .and_then(|record| record.get("Subject"))
            .unwrap_or_else(|| {
                panic!("case {case:?}: every record carries a Subject member, got {record}")
            });
        assert_eq!(
            subject,
            &row.subject.map_or(Value::Null, Value::from),
            "case {case:?}: expected the subject {:?}, got {subject}",
            row.subject
        );
    }
}
