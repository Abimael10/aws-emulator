// SPDX-License-Identifier: CC0-1.0

//! `Action$dynamoDBv2`, "Write to a DynamoDB table ... It allows you to write each attribute in
//! an MQTT message payload into a separate DynamoDB column": the item one selected message is
//! written as, and the hop the rules engine writes it over.
//!
//! The item is the MESSAGE ITSELF, member for member: "Each attribute in the message payload
//! will be written to a separate column in the DynamoDB database"
//! (`DynamoDBv2Action$putItem`), so a top-level member of the selected document is one attribute
//! of the item under its own name, of the DynamoDB type its JSON value is
//! ([`attribute_value`]) — which is what makes the table's own partition key a column the
//! message supplies, and what a later `GetItem` on that key reads back.
//!
//! What decides whether the write is made at all is the action's role: `dynamoDBv2` names "the
//! ARN of the IAM role that grants access to the DynamoDB table", AWS IoT acts AS that role, and
//! the call it makes is [`PUT_ITEM_ACTION`] on the table the action names
//! ([`super::super::rule_action_role`]). A role that does not admit this service principal or
//! does not grant that call is a write NOT made, which is the only observable the permission
//! boundary has, and which the rule's `errorAction` reports.
//!
//! DynamoDB's own refusal of the write — a table the account does not hold, one not yet
//! `ACTIVE`, a message carrying no value for the table's key — is the action FAILING, and it is
//! reported in DynamoDB's own words ([`super::refusal_of`]) rather than in a sentence worded
//! here.
//!
//! The call goes over the emulator's own IN-PROCESS transport ([`dynamodb_client`]), so the
//! table a rule names reaches only what this emulator serves itself: no socket is opened and no
//! host name is resolved.

use ::std::collections::HashMap;

use ::aws_sdk_dynamodb::types::AttributeValue;
use ::serde_json::Value;

/// The IAM action a `dynamoDBv2` rule action performs on the table it names: `PutItem`
/// "requires permission for the `dynamodb:PutItem` action".
pub const PUT_ITEM_ACTION: &str = "dynamodb:PutItem";

/// Amazon DynamoDB's ARN namespace and the resource type of a table:
/// `arn:aws:dynamodb:<region>:<account>:table/<tableName>`, the resource an action's role is
/// granted [`PUT_ITEM_ACTION`] on.
const DYNAMODB_ARN_NAMESPACE: &str = "dynamodb";
const TABLE_RESOURCE_TYPE: &str = "table";

/// The shape of the action this engine names in a rule-failure report, as the model names it.
pub const FAILED_ACTION: &str = "DynamoDBv2Action";

/// The ARN of the table `table_name` in `region`, which is the resource the action's role has to
/// be granted the write on.
#[must_use]
pub fn table_arn(region: &str, table_name: &str) -> String {
    ::emulator_server::emulator_arn(
        DYNAMODB_ARN_NAMESPACE,
        region,
        &format!("{TABLE_RESOURCE_TYPE}/{table_name}"),
    )
}

/// Write one item for the selected message `message` into the table `table_name`.
///
/// `Err` is why the write was NOT made, worded by whatever refused it — DynamoDB's own message,
/// which is what the rule's `errorAction` reports.
pub async fn put_item(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    table_name: &str,
    message: &[u8],
) -> Result<(), String> {
    let written = dynamodb_client(ctx, region)
        .put_item()
        .table_name(table_name)
        .set_item(Some(item_of(message)))
        .send()
        .await;
    written.map(|_| ()).map_err(|error| super::refusal_of(&error))
}

/// The item the selected message is written as: one attribute per TOP-LEVEL member of the
/// document, under that member's own name.
///
/// A payload that is no JSON document carries no members, so it is written as no attributes at
/// all — and DynamoDB then refuses the write for the key it was given no value for, which is
/// its answer to make rather than this engine's.
fn item_of(message: &[u8]) -> HashMap<String, AttributeValue> {
    let Ok(Value::Object(document)) = ::serde_json::from_slice::<Value>(message) else {
        return HashMap::new();
    };
    document
        .into_iter()
        .map(|(name, value)| (name, attribute_value(&value)))
        .collect()
}

/// The DynamoDB attribute one JSON value is written as: the type the value HAS, so a string is
/// `S`, a number `N` (its digits as the document writes them, which is the form DynamoDB carries
/// a number in), a boolean `BOOL`, a `null` `NULL`, an array `L` of its items and an object `M`
/// of its members — each member of a nested value written the same way, so the item read back is
/// the document the rule selected.
fn attribute_value(value: &Value) -> AttributeValue {
    match value {
        Value::String(text) => AttributeValue::S(text.clone()),
        Value::Number(number) => AttributeValue::N(number.to_string()),
        Value::Bool(flag) => AttributeValue::Bool(*flag),
        Value::Null => AttributeValue::Null(true),
        Value::Array(items) => AttributeValue::L(items.iter().map(attribute_value).collect()),
        Value::Object(members) => AttributeValue::M(
            members
                .iter()
                .map(|(name, member)| (name.clone(), attribute_value(member)))
                .collect(),
        ),
    }
}

/// The Amazon DynamoDB client a rule action writes an item through, addressed to `region`.
///
/// Configured as every other in-process client in this tree is
/// (`::service_common::kinesis_client`): the callback routes IN PROCESS to this emulator's own
/// inbound listener, so the endpoint URL is addressing for that transport rather than a host
/// anything connects to, and the credentials are the emulator's own, which sign this hop and
/// never appear in a response. There is nothing to retry, time out or watch for a stalled
/// stream over an in-process transport, and each of those would need a sleep implementation this
/// client is not given.
///
/// Built in this crate rather than in `services/common` for the reason the sibling copies state
/// (`service-com-amazonaws-backup-cryocontrollerusermanager`'s `state::restore_execution`): that
/// crate is a dependency of EVERY service crate, so the DynamoDB SDK declared there would be
/// compiled into every one of them for the sake of this one constructor.
fn dynamodb_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_dynamodb::Client {
    let config = ::aws_sdk_dynamodb::Config::builder()
        .behavior_version(::aws_sdk_dynamodb::config::BehaviorVersion::latest())
        .region(::aws_sdk_dynamodb::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://dynamodb.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        .retry_config(::aws_sdk_dynamodb::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_dynamodb::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_dynamodb::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_dynamodb::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_dynamodb::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: what the case is, the payload published, and the attributes
    /// the item must then carry.
    type Row<'a> = (&'a str, &'a str, &'a [(&'a str, AttributeValue)]);

    #[test]
    fn each_top_level_member_of_the_message_is_one_attribute_of_the_item() {
        // The rows differ along the JSON type of the member's value — every type a document
        // carries — plus a document with no members at all and a payload that is no document.
        let rows: &[Row<'_>] = &[
            (
                "a string, a number and a boolean member",
                r#"{"id":"m-1","temp":42,"ok":true}"#,
                &[
                    ("id", AttributeValue::S("m-1".to_string())),
                    ("temp", AttributeValue::N("42".to_string())),
                    ("ok", AttributeValue::Bool(true)),
                ],
            ),
            (
                "a fractional number, which keeps the digits the document writes",
                r#"{"ratio":0.25}"#,
                &[("ratio", AttributeValue::N("0.25".to_string()))],
            ),
            (
                "a null member",
                r#"{"missing":null}"#,
                &[("missing", AttributeValue::Null(true))],
            ),
            (
                "an array member, which is a list of its items",
                r#"{"tags":["a",1]}"#,
                &[(
                    "tags",
                    AttributeValue::L(vec![
                        AttributeValue::S("a".to_string()),
                        AttributeValue::N("1".to_string()),
                    ]),
                )],
            ),
            (
                "an object member, which is a map of its own members",
                r#"{"state":{"room":"r1"}}"#,
                &[(
                    "state",
                    AttributeValue::M(
                        [("room".to_string(), AttributeValue::S("r1".to_string()))]
                            .into_iter()
                            .collect(),
                    ),
                )],
            ),
            ("a document with no members", "{}", &[]),
            ("a payload that is no document", "not a document", &[]),
            ("a document that is no object", "[1,2]", &[]),
        ];
        for (case, payload, expected) in rows {
            let item = item_of(payload.as_bytes());
            assert_eq!(
                item.len(),
                expected.len(),
                "case {case:?}: expected {} attributes, got {item:?}",
                expected.len()
            );
            for (name, value) in *expected {
                let actual = item.get(*name);
                assert_eq!(
                    actual,
                    Some(value),
                    "case {case:?}: expected {name} to be {value:?}, got {actual:?}"
                );
            }
        }
    }
}
