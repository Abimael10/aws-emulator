// SPDX-License-Identifier: CC0-1.0

//! `AWS::IoT::TopicRule`: the topic rule a stack declares, created in AWS IoT itself.
//!
//! This is the per-type half of provisioning for one resource type, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::iot_authorizer`] for the reason that module records —
//! the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the rule is really created
//!
//! A topic rule is not a record: it is a LIVE subscription of the message broker. Every
//! message published to a topic the rule's SQL `FROM` clause covers runs the rule's actions
//! (`service_com_amazonaws_iot_awsiotservice::state::rules_engine`), and the rule's own ARN —
//! `arn:aws:iot:<region>:<account>:rule/<ruleName>` — is the `aws:SourceArn` a target
//! function's resource policy grant is narrowed by. So a stack that reported a rule created
//! while nothing created it deploys an INERT rule: the publish is accepted, the actions never
//! run, and a template that omits the grant its actions need is indistinguishable from one
//! that carries it. Creating the rule where it belongs is what makes the deployed estate
//! behave as the API-created one does, and `GetTopicRule`, `ListTopicRules`,
//! `EnableTopicRule`, `DisableTopicRule` and `ReplaceTopicRule` all address it by the name it
//! was created under.
//!
//! ## The properties
//!
//! The type publishes `RuleName` — the name the rule is created UNDER, which is this
//! resource's physical id and what its `Ref` resolves to — the `TopicRulePayload` the create
//! takes, and `Tags`. A declaration that names no rule is created under the identifier
//! CloudFormation computed for the resource.
//!
//! `TopicRulePayload` is `CreateTopicRule`'s own payload shape with every member's initial
//! upper-cased, all the way down (`Sql`, `Actions`, `RuleDisabled`, `AwsIotSqlVersion`,
//! `ErrorAction`, and inside an action `Lambda.FunctionArn`, `Sqs.QueueUrl`,
//! `Republish.Topic`), which is the one rule this arm maps the declaration by
//! ([`api_document`]) — so every action type the payload models reaches the owning service
//! under that service's own member names rather than through a per-action table that would
//! silently drop the ones it had not been taught.
//!
//! A declared property this arm does not apply is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a rule reported created with less than its declaration
//! asked for is the shallow success this arm exists to avoid.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    applicable_tags, call_rest_json, lower_camel, percent_encoded, upper_camel, Created, Readiness,
    Refusal, Removal,
};
use crate::state::provisioner::tags_of;
use crate::state::StoredResource;

/// The resource type this module provisions.
pub(crate) const IOT_TOPIC_RULE: &str = "AWS::IoT::TopicRule";

/// The property naming the rule, which is the name it is created under rather than a member of
/// the create's body — the type's own name property, which the convention
/// (`<last segment>Name`, so `TopicRuleName`) does not find.
pub(crate) const RULE_NAME: &str = "RuleName";

/// The SigV4 signing name of the service that owns it, which is what routes the in-process
/// call to AWS IoT's own handlers (that model's `sigv4Name`/`endpointPrefix`,
/// `smithy_query(query="service_info")`). It speaks `restJson1`, so each call is a method and
/// a URI rather than an `X-Amz-Target`.
const IOT_SIGNING_NAME: &str = "iot";

/// The code AWS IoT answers a read of a rule it does not hold with.
///
/// `GetTopicRule` models no not-found error at all
/// (`smithy_query(query="operation",operation="GetTopicRule")`): its client errors are
/// `InvalidRequestException` and `UnauthorizedException`, and a rule the account does not hold
/// is the latter — so for a call made as this emulator's own credentials, which every
/// in-process call is, that code means the rule is not there.
const RULE_NOT_READABLE: &str = "UnauthorizedException";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that states none of a required property.
const MISSING_PROPERTY: &str = "InvalidRequest";

/// The property carrying everything about the rule but its name, which is `CreateTopicRule`'s
/// own `@required` `topicRulePayload`.
const TOPIC_RULE_PAYLOAD: &str = "TopicRulePayload";

/// The attribute `AWS::IoT::TopicRule` publishes through `Fn::GetAtt` ("Arn: the Amazon
/// Resource Name (ARN) of the topic rule"), read off the member the rule's own read answers it
/// under.
const ARN_ATTRIBUTE: &str = "Arn";
const RULE_ARN_MEMBER: &str = "ruleArn";

/// The member `GetTopicRule` answers the rule's own definition under, whose members are
/// `TopicRulePayload`'s.
const RULE_MEMBER: &str = "rule";

/// Every property `AWS::IoT::TopicRule` publishes. A declared property this list does not
/// carry is one no call here applies, and is refused rather than dropped.
const TOPIC_RULE_PROPERTIES: &[&str] = &[RULE_NAME, TOPIC_RULE_PAYLOAD, "Tags"];

/// The two members AWS IoT's own `Tag` structure spells a tag's key and value with
/// (`smithy_query(query="shape",shape="Tag")`).
const TAG_MEMBERS: (&str, &str) = ("Key", "Value");

/// The members of the payload tree the API models as BOOLEANS, which a template may write as a
/// quoted scalar or feed through a `Ref` to a `String` parameter — every value of one arrives
/// as text.
///
/// Source: the boolean members of `TopicRulePayload`'s own tree —
/// `TopicRulePayload$ruleDisabled`, `SqsAction$useBase64` and `HttpAction$enableBatching`.
const PAYLOAD_FLAGS: &[&str] = &["RuleDisabled", "UseBase64", "EnableBatching"];

/// The members of the payload tree the API models as INTEGERS, for the same reason.
///
/// Source: `RepublishAction$qos` (`Qos`, `range 0..1`), the one integer a declaration of this
/// type's payload states.
const PAYLOAD_NUMBERS: &[&str] = &["Qos"];

/// The members whose value is a map whose KEYS are the caller's own rather than the model's, so
/// the key-case rule [`api_document`] applies must not touch them.
///
/// Source: `KafkaAction$clientProperties`, the one `map` of caller-chosen keys in
/// `TopicRulePayload`'s tree (`smithy_query(query="shape",shape="KafkaAction")`); every other
/// nested member of that tree is a structure or a list of structures, whose keys ARE the
/// model's.
const CALLER_KEYED_MEMBERS: &[&str] = &["ClientProperties"];

/// How deep a declared payload is mapped. The declaration is a caller's document, so its
/// nesting is caller-controlled, while `TopicRulePayload`'s own tree is a handful of levels deep
/// (the deepest the model has is `payload -> actions -> action -> dynamoDBv2 -> putItem ->
/// tableName`). Far above any declaration of this type, and what keeps the walk below finite
/// whatever a template nested: past it the value is carried exactly as written, so nothing of
/// the declaration is dropped and the owning service's own validation is what judges it.
const MAX_PROPERTY_DEPTH: usize = 32;

/// Create the rule an `AWS::IoT::TopicRule` declares, in AWS IoT itself — so a publish to a
/// topic the deployed rule covers runs its actions exactly as an API-created rule's do, and a
/// second stack declaring the same name is refused by the service that owns the name.
///
/// `Err` is the owning service's own refusal of the create, which is what the resource fails
/// with: a name the account already holds, a SQL statement it cannot parse, a rule count past
/// the account's limit.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        TOPIC_RULE_PROPERTIES,
    )?;
    let name = resource.physical_resource_id.clone();
    let payload = declared_payload(&resource.properties, &resource.resource_type)?;
    call_iot(ctx, region, "POST", &rule_uri(&name), Some(&payload)).await?;

    // The rule's own ARN is what the type publishes and what a grant for its actions is
    // narrowed by, and `CreateTopicRule` answers no body at all (its output is `Unit`), so it
    // is read back from the service that minted it rather than built here.
    let attributes = read_attributes(ctx, region, &name).await?;

    // The rule carries the tags its declaration asks for plus the ones CloudFormation gives
    // the resource — less the keys in AWS's reserved namespace, which this service documents
    // as its own ("Do not use the `aws:` prefix ... it is reserved for AWS use") and which no
    // public call of it writes. `CreateTopicRule` takes them in an `x-amz-tagging` HEADER, so
    // they are applied through `TagResource`, the call that takes a tag LIST and addresses the
    // rule by the ARN above.
    if let Some(arn) = attributes.get(ARN_ATTRIBUTE) {
        if let Err(refusal) = apply_tags(ctx, region, arn, &resource.properties, propagated).await {
            // A rule tagged differently from its declaration is not a resource the stack may
            // report as created, so it is removed again before the refusal is reported.
            let _ = call_iot(ctx, region, "DELETE", &rule_uri(&name), None).await;
            return Err(refusal);
        }
    }
    Ok(Created::attributes(attributes))
}

/// Whether the owning service reports the created rule ready.
///
/// Nothing is asked, and nothing waits: a rule is LIVE from the moment `CreateTopicRule`
/// answers — `GetTopicRule` resolves it and the next matching publish runs its actions — and
/// whether it is enabled is what the declaration's own `RuleDisabled` asked for rather than a
/// transient state it settles out of.
pub(super) fn ready() -> Readiness {
    Readiness::Ready
}

/// Whether the owning service has finished removing a deleted rule.
///
/// Nothing is asked: `DeleteTopicRule` takes the rule away as it answers, and AWS IoT reports
/// no state a deleted one is still being removed in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the rule the stack already has, through `ReplaceTopicRule` —
/// "replaces the rule" by the name it was created under, so the rule keeps its identity and
/// its ARN, and nothing is replaced.
///
/// The rule's NAME is its identity, and a declaration that changes it is a different resource:
/// [`super::super::template`] computes a new physical id for it, so the deployment replaces the
/// rule rather than reaching here.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    // The DECLARED document, not the record's: by the time this is reached the resource
    // carries the properties it was CREATED with, so a property the new declaration
    // introduces is only in `declared`.
    refuse_unapplied_properties(declared, &resource.resource_type, TOPIC_RULE_PROPERTIES)?;
    let name = resource.physical_resource_id.clone();
    let payload = declared_payload(declared, &resource.resource_type)?;
    call_iot(ctx, region, "PATCH", &rule_uri(&name), Some(&payload)).await?;
    let attributes = read_attributes(ctx, region, &name).await?;
    if let Some(arn) = attributes.get(ARN_ATTRIBUTE) {
        apply_tags(ctx, region, arn, declared, &[]).await?;
    }
    Ok(Created::attributes(attributes))
}

/// Delete the rule a stack created, addressed by the name it was created under.
///
/// `DeleteTopicRule` is answered the same way for a rule the account no longer holds — there is
/// nothing left to remove — so a teardown that runs twice is not a failure.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    call_iot(
        ctx,
        region,
        "DELETE",
        &rule_uri(&resource.physical_resource_id),
        None,
    )
    .await
    .map(|_| ())
}

/// The rule's configuration as the owning service reports it, for drift detection.
///
/// `GetTopicRule` answers a `rule` whose members are `TopicRulePayload`'s own, so the answer
/// maps back onto the property vocabulary by the same key-case rule the create maps forward by
/// ([`property_document`]). A rule the owning service no longer reads is one the stack no
/// longer has ([`RULE_NOT_READABLE`]).
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let answered = match read_rule(ctx, region, &resource.physical_resource_id).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == RULE_NOT_READABLE => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let Some(reported) = answered.get(RULE_MEMBER) else {
        return Ok(None);
    };
    let mut configuration = Map::new();
    configuration.insert(
        RULE_NAME.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    configuration.insert(
        TOPIC_RULE_PAYLOAD.to_string(),
        property_document(reported),
    );
    Ok(Some(Value::Object(configuration)))
}

/// The payload the create and the replace carry: the declaration's own `TopicRulePayload`,
/// mapped onto the API's member names ([`api_document`]).
///
/// A declaration stating none describes no rule that could be created — `topicRulePayload` is
/// `@required` and so are its `sql` and `actions` — so it is refused here rather than sent as a
/// request the owning service would reject.
fn declared_payload(properties: &Value, resource_type: &str) -> Result<Value, Refusal> {
    let declared = properties
        .get(TOPIC_RULE_PAYLOAD)
        .filter(|value| value.is_object())
        .ok_or_else(|| Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!("{resource_type} declares no {TOPIC_RULE_PAYLOAD}"),
        })?;
    Ok(api_document(declared))
}

/// The attributes the type publishes for the rule `name`: the ARN AWS IoT minted for it, read
/// from that service's own read of the rule.
async fn read_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let answered = read_rule(ctx, region, name).await?;
    let mut attributes = BTreeMap::new();
    if let Some(arn) = answered
        .get(RULE_ARN_MEMBER)
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
    {
        attributes.insert(ARN_ATTRIBUTE.to_string(), arn.to_string());
    }
    Ok(attributes)
}

/// `GetTopicRule` for `name`.
async fn read_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<Value, Refusal> {
    call_iot(ctx, region, "GET", &rule_uri(name), None).await
}

/// Put the tags the declaration and the stack give the rule onto it, through `TagResource`.
///
/// Nothing is called for a rule that carries no tags at all, so a stack with no tags makes no
/// extra call.
async fn apply_tags(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    rule_arn: &str,
    properties: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let tags = applicable_tags(&tags_of(properties, propagated));
    if tags.is_empty() {
        return Ok(());
    }
    let mut request = Map::new();
    request.insert(
        "resourceArn".to_string(),
        Value::String(rule_arn.to_string()),
    );
    request.insert(
        "tags".to_string(),
        Value::Array(
            tags.into_iter()
                .map(|(key, value)| {
                    let mut tag = Map::new();
                    tag.insert(TAG_MEMBERS.0.to_string(), Value::String(key));
                    tag.insert(TAG_MEMBERS.1.to_string(), Value::String(value));
                    Value::Object(tag)
                })
                .collect(),
        ),
    );
    call_iot(ctx, region, "POST", "/tags", Some(&Value::Object(request)))
        .await
        .map(|_| ())
}

/// The URI the per-rule operations are bound to (`/rules/{ruleName}`, that model's own `http`
/// binding), with the name written as ONE path segment so a name outside the modelled pattern
/// is refused by that operation's validation instead of addressing a different one.
fn rule_uri(name: &str) -> String {
    format!("/rules/{}", percent_encoded(name))
}

/// `declared` as the API takes it: every object key's initial LOWER-cased, recursively
/// ([`super::lower_camel`], this crate's one spelling of that rule).
///
/// A CloudFormation resource type's properties are the API members of the resource it wraps
/// with their initial upper-cased, and `AWS::IoT::TopicRule` wraps `CreateTopicRule`'s payload
/// whole — so that one rule carries the entire tree, from `Sql` and `Actions` down through
/// `Lambda.FunctionArn`, `Republish.Topic` and `DynamoDBv2.PutItem.TableName`. Mapping by the
/// rule rather than by a per-member table is what makes every action type the payload models
/// reach the owning service, including the ones no table here was taught.
///
/// Two things it does NOT do: a member whose map keys are the CALLER's
/// ([`CALLER_KEYED_MEMBERS`]) keeps its keys exactly as declared, and a scalar the API models
/// as a flag or a number is read as that ([`PAYLOAD_FLAGS`], [`PAYLOAD_NUMBERS`]) so a template
/// that wrote one as text — which is every value a `Ref` to a `String` parameter yields — still
/// states the value the API takes. Everything else travels as the template wrote it, so the
/// owning service's own validation is what judges it.
///
/// The walk is bounded by [`MAX_PROPERTY_DEPTH`], because the document it walks is a caller's.
fn api_document(declared: &Value) -> Value {
    api_members(declared, 0)
}

/// [`api_document`] at depth `depth`, which stops at [`MAX_PROPERTY_DEPTH`].
fn api_members(declared: &Value, depth: usize) -> Value {
    if depth >= MAX_PROPERTY_DEPTH {
        return declared.clone();
    }
    match declared {
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(property, value)| {
                    let member = lower_camel(property);
                    (member, declared_member(property, value, depth))
                })
                .collect(),
        ),
        Value::Array(elements) => Value::Array(
            elements
                .iter()
                .map(|element| api_members(element, depth + 1))
                .collect(),
        ),
        scalar => scalar.clone(),
    }
}

/// One declared member's value as the API takes it: the flag or number it is where the API
/// models one, its own keys where they are the caller's, and otherwise the same mapping applied
/// to whatever it holds.
fn declared_member(property: &str, value: &Value, depth: usize) -> Value {
    if CALLER_KEYED_MEMBERS.contains(&property) {
        return value.clone();
    }
    if PAYLOAD_FLAGS.contains(&property) {
        return declared_flag(value).map_or_else(|| value.clone(), Value::Bool);
    }
    if PAYLOAD_NUMBERS.contains(&property) {
        return declared_number(value).unwrap_or_else(|| value.clone());
    }
    api_members(value, depth + 1)
}

/// `reported` as the property vocabulary spells it: the inverse of [`api_document`]'s key-case
/// rule ([`super::upper_camel`]), so the configuration a drift read compares is in the same
/// vocabulary the declaration is.
fn property_document(reported: &Value) -> Value {
    property_members(reported, 0)
}

/// [`property_document`] at depth `depth`, bounded as [`api_members`] is: the document is the
/// owning service's own answer about a rule a caller stored, so its nesting is caller-controlled
/// too.
fn property_members(reported: &Value, depth: usize) -> Value {
    if depth >= MAX_PROPERTY_DEPTH {
        return reported.clone();
    }
    match reported {
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(member, value)| {
                    (upper_camel(member), property_members(value, depth + 1))
                })
                .collect(),
        ),
        Value::Array(elements) => Value::Array(
            elements
                .iter()
                .map(|element| property_members(element, depth + 1))
                .collect(),
        ),
        scalar => scalar.clone(),
    }
}

/// A declared boolean in either spelling a template gives one: JSON's boolean, or the text a
/// template spells one as, read case-insensitively. `None` is a value that is no boolean at
/// all, which the owning service's own validation is what judges, so it is left exactly as
/// written.
fn declared_flag(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) if text.eq_ignore_ascii_case("true") => Some(true),
        Value::String(text) if text.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// A declared number in either spelling a template gives one: JSON's number, or the text a
/// template spells one as. `None` is a value that is no number at all, left exactly as written
/// for the same reason.
fn declared_number(value: &Value) -> Option<Value> {
    match value {
        Value::Number(_) => Some(value.clone()),
        Value::String(text) => text
            .trim()
            .parse::<i64>()
            .ok()
            .map(|number| Value::Number(number.into())),
        _ => None,
    }
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a rule from being reported as created — or as updated — with
/// less than its declaration asked for.
fn refuse_unapplied_properties(
    properties: &Value,
    resource_type: &str,
    published: &[&str],
) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    for property in properties.keys() {
        if published.contains(&property.as_str()) {
            continue;
        }
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        });
    }
    Ok(())
}

/// Call one AWS IoT operation on the REST-JSON wire its model declares.
async fn call_iot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    method: &'static str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json(ctx, IOT_SIGNING_NAME, region, method, uri, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared payload reaches `CreateTopicRule` under that call's own member names, all
    /// the way down the action tree — and a flag or a number written as text reaches it as the
    /// value the API models.
    ///
    /// The rows are the members a declaration states: the statement, an action of each of three
    /// target services (one of which nests a structure), the error action, and the two scalars
    /// the API does not model as strings.
    #[test]
    fn the_declared_payload_is_carried_under_the_api_member_names() {
        let declared = serde_json::json!({
            "Sql": "SELECT * FROM 'sensors/+/temp'",
            "Description": "forward every reading",
            "RuleDisabled": "false",
            "AwsIotSqlVersion": "2016-03-23",
            "Actions": [
                {"Lambda": {"FunctionArn": "arn:aws:lambda:us-east-1:000000000000:function:f"}},
                {"Sqs": {
                    "RoleArn": "arn:aws:iam::000000000000:role/r",
                    "QueueUrl": "https://sqs.us-east-1.amazonaws.com/000000000000/q",
                    "UseBase64": true,
                }},
                {"Republish": {
                    "RoleArn": "arn:aws:iam::000000000000:role/r",
                    "Topic": "staged/temp",
                    "Qos": "1",
                }},
                {"DynamoDBv2": {
                    "RoleArn": "arn:aws:iam::000000000000:role/r",
                    "PutItem": {"TableName": "readings"},
                }},
            ],
            "ErrorAction": {"Lambda": {
                "FunctionArn": "arn:aws:lambda:us-east-1:000000000000:function:fallback",
            }},
        });
        let carried = api_document(&declared);
        let expected = serde_json::json!({
            "sql": "SELECT * FROM 'sensors/+/temp'",
            "description": "forward every reading",
            "ruleDisabled": false,
            "awsIotSqlVersion": "2016-03-23",
            "actions": [
                {"lambda": {"functionArn": "arn:aws:lambda:us-east-1:000000000000:function:f"}},
                {"sqs": {
                    "roleArn": "arn:aws:iam::000000000000:role/r",
                    "queueUrl": "https://sqs.us-east-1.amazonaws.com/000000000000/q",
                    "useBase64": true,
                }},
                {"republish": {
                    "roleArn": "arn:aws:iam::000000000000:role/r",
                    "topic": "staged/temp",
                    "qos": 1,
                }},
                {"dynamoDBv2": {
                    "roleArn": "arn:aws:iam::000000000000:role/r",
                    "putItem": {"tableName": "readings"},
                }},
            ],
            "errorAction": {"lambda": {
                "functionArn": "arn:aws:lambda:us-east-1:000000000000:function:fallback",
            }},
        });
        assert_eq!(
            carried, expected,
            "case \"a full payload\": expected {expected}, got {carried}"
        );
    }

    /// A member whose map keys are the CALLER's keeps them exactly as declared: the rule that
    /// lowers an initial is about the model's own member names, and a caller's key is not one.
    #[test]
    fn a_callers_own_map_keys_are_left_alone() {
        let declared = serde_json::json!({
            "Kafka": {
                "DestinationArn": "arn:aws:kafka:us-east-1:000000000000:cluster/c/1",
                "Topic": "Readings",
                "ClientProperties": {"Acks": "all", "bootstrap.servers": "Broker:9092"},
            },
        });
        let carried = api_document(&declared);
        let expected = serde_json::json!({
            "kafka": {
                "destinationArn": "arn:aws:kafka:us-east-1:000000000000:cluster/c/1",
                "topic": "Readings",
                "clientProperties": {"Acks": "all", "bootstrap.servers": "Broker:9092"},
            },
        });
        assert_eq!(
            carried, expected,
            "case \"a caller-keyed map\": expected {expected}, got {carried}"
        );
    }

    /// The two halves of the key-case rule are inverses, so the configuration a drift read
    /// reports is in the vocabulary the declaration is written in.
    #[test]
    fn the_reported_rule_maps_back_onto_the_property_vocabulary() {
        let reported = serde_json::json!({
            "ruleName": "forwarder",
            "sql": "SELECT * FROM 'sensors/+/temp'",
            "ruleDisabled": false,
            "actions": [{"lambda": {"functionArn": "arn:aws:lambda:us-east-1:0:function:f"}}],
        });
        let properties = property_document(&reported);
        let expected = serde_json::json!({
            "RuleName": "forwarder",
            "Sql": "SELECT * FROM 'sensors/+/temp'",
            "RuleDisabled": false,
            "Actions": [{"Lambda": {"FunctionArn": "arn:aws:lambda:us-east-1:0:function:f"}}],
        });
        assert_eq!(
            properties, expected,
            "case \"a reported rule\": expected {expected}, got {properties}"
        );
        let back = api_document(&properties);
        assert_eq!(
            back, reported,
            "case \"a round trip through both halves\": expected {reported}, got {back}"
        );
    }

    /// The URI is the model's own binding with the name as ONE path segment, so a name
    /// carrying a character that is structure in a URI cannot address another operation.
    #[test]
    fn the_uri_carries_the_name_as_one_segment() {
        let cases: &[(&str, &str, &str)] = &[
            ("an ordinary name", "forwarder", "/rules/forwarder"),
            ("a name with a slash", "a/b", "/rules/a%2Fb"),
            ("a name with a question mark", "a?b", "/rules/a%3Fb"),
        ];
        for (case, name, expected) in cases {
            let actual = rule_uri(name);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the URI {expected:?}, got {actual:?}"
            );
        }
    }

    /// A property no call here applies is refused, naming it; every property the type
    /// publishes is accepted. A declaration stating no payload at all describes no rule.
    #[test]
    fn an_unapplied_property_is_refused_and_so_is_a_declaration_with_no_payload() {
        let declared = serde_json::json!({
            "RuleName": "forwarder",
            "TopicRulePayload": {"Sql": "SELECT * FROM 'a'", "Actions": []},
            "Tags": [],
        });
        assert!(
            refuse_unapplied_properties(&declared, IOT_TOPIC_RULE, TOPIC_RULE_PROPERTIES).is_ok(),
            "case \"every published property\": expected it to be applied, got a refusal"
        );
        let unknown = serde_json::json!({"RuleName": "forwarder", "Unheard": 1});
        let refusal =
            refuse_unapplied_properties(&unknown, IOT_TOPIC_RULE, TOPIC_RULE_PROPERTIES)
                .expect_err("case \"a property no call carries\": expected a refusal");
        assert!(
            refusal.message.contains("Unheard"),
            "case \"a property no call carries\": expected the refusal to name it, got {:?}",
            refusal.message
        );
        let empty = serde_json::json!({"RuleName": "forwarder"});
        let refusal = declared_payload(&empty, IOT_TOPIC_RULE)
            .expect_err("case \"a declaration with no payload\": expected a refusal");
        assert!(
            refusal.message.contains(TOPIC_RULE_PAYLOAD),
            "case \"a declaration with no payload\": expected the refusal to name the \
             property, got {:?}",
            refusal.message
        );
    }
}
