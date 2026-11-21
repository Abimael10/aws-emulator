// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Amazon CloudWatch composite alarm an
//! `AWS::CloudWatch::CompositeAlarm` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! ## Why the alarm is really created
//!
//! "Ref returns the alarm name" and `Fn::GetAtt` reads its ARN, and that name is the handle
//! every CloudWatch call about an alarm takes: `DescribeAlarms`, `SetAlarmState`,
//! `EnableAlarmActions`, `DeleteAlarms` and a rollback trigger's own monitoring each address
//! one by it. A composite alarm is also the only resource that DERIVES its state from other
//! alarms — "The alarm rule to use to determine the alarm state" (`PutCompositeAlarm`) — so a
//! stack that reported the resource created while nothing put the alarm publishes a name that
//! resolves to nothing, and the roll-up the template deploys evaluates nowhere.
//!
//! ## The properties
//!
//! The type's properties ARE `PutCompositeAlarm`'s own members, spelled identically on both
//! sides ([`COMPOSITE_ALARM_PROPERTIES`]), with the alarm's NAME the one member CloudFormation
//! decides — it is this resource's physical id. So the resolved property document is the
//! request, and the one call serves this type's create and its in-place update alike: "Creates
//! or updates a composite alarm" (`PutCompositeAlarm`).
//!
//! A template's scalar is TEXT wherever its author quoted it and wherever a `Ref` to a
//! parameter resolved it, while that call models a switch and the two suppression periods as a
//! boolean and as numbers — so every member it types as one is read as that type before the
//! request is sent ([`composite_alarm_scalar`]). Without it the put is refused by its own
//! deserializer, which fails the resource and rolls the whole stack back.
//!
//! The alarm carries the tags its own declaration asks for plus the ones CloudFormation gives
//! the resource ([`tags_of`]), as the `[{Key, Value}]` list CloudWatch's own `Tag` shape
//! spells — the shape that call's own `Tags` member takes.
//!
//! A declared property none of that call's members carries is REFUSED rather than dropped
//! ([`refuse_unapplied_composite_properties`]): an alarm reported created with less than its
//! declaration asked for is the shallow success this arm exists to avoid.

use super::*;

/// The resource type this module provisions.
pub(in crate::state) const CLOUDWATCH_COMPOSITE_ALARM: &str = "AWS::CloudWatch::CompositeAlarm";

/// The operations this module calls, as Amazon CloudWatch's own model names them.
const PUT_COMPOSITE_ALARM: &str = "PutCompositeAlarm";
const DELETE_ALARMS: &str = "DeleteAlarms";
const DESCRIBE_ALARMS: &str = "DescribeAlarms";

/// The error code CloudWatch answers for an alarm it does not hold (`ResourceNotFound`, the
/// 404 `DeleteAlarms` models), which is what says a delete has nothing left to remove.
const ALARM_NOT_FOUND: &str = "ResourceNotFound";

/// The member that names the alarm on the put, and the member a read addresses alarms by.
pub(in crate::state) const COMPOSITE_ALARM_NAME_MEMBER: &str = "AlarmName";
const ALARM_NAMES_MEMBER: &str = "AlarmNames";

/// The member `DescribeAlarms` answers the composite alarms it holds in.
const COMPOSITE_ALARMS_MEMBER: &str = "CompositeAlarms";

/// The property carrying the alarm's own tag set, merged with the ones CloudFormation
/// propagates ([`tags_of`]) and rebuilt rather than carried.
const TAGS_PROPERTY: &str = "Tags";

/// Every property `AWS::CloudWatch::CompositeAlarm` publishes, each a member of
/// `PutCompositeAlarm` under the same name, in the order AWS's resource reference lists them.
///
/// Source: that reference's property list against `PutCompositeAlarm`'s member list
/// (`smithy_query(query="operation",operation="PutCompositeAlarm")`). The alarm's own
/// `AlarmName` is in it because a template may state one; it travels as the NAME the resource
/// was given rather than as a member of its own ([`put_composite_alarm`]).
const COMPOSITE_ALARM_PROPERTIES: &[&str] = &[
    "ActionsEnabled",
    "ActionsSuppressor",
    "ActionsSuppressorExtensionPeriod",
    "ActionsSuppressorWaitPeriod",
    "AlarmActions",
    "AlarmDescription",
    COMPOSITE_ALARM_NAME_MEMBER,
    "AlarmRule",
    "InsufficientDataActions",
    "OKActions",
    TAGS_PROPERTY,
];

/// The SCALAR properties a drift read reports back, each the member of the same name on
/// `CompositeAlarm`.
///
/// A property that is a LIST on the wire (the three action lists) is left out, which is the
/// position every arm whose property is not a scalar is in and what [`super::Provisioner`]'s
/// drift read documents.
const REPORTED_COMPOSITE_PROPERTIES: &[&str] = &[
    "ActionsEnabled",
    "ActionsSuppressor",
    "ActionsSuppressorExtensionPeriod",
    "ActionsSuppressorWaitPeriod",
    "AlarmDescription",
    "AlarmRule",
];

/// Create — or update — the composite alarm an `AWS::CloudWatch::CompositeAlarm` declares, in
/// Amazon CloudWatch itself: so a client that deployed the stack reads the alarm back with
/// `cloudwatch describe-alarms`, and the rule the template declared is the one that service
/// evaluates over the children the same stack created.
///
/// `Err` is CloudWatch's own refusal of the put, which is what the resource fails with: an
/// alarm rule that does not parse, a suppressor naming an alarm that does not exist.
pub(super) async fn put_composite_alarm(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_composite_properties(&resource.properties, &resource.resource_type)?;
    let mut properties = match &resource.properties {
        Value::Object(object) => object.clone(),
        _ => Map::new(),
    };
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        properties.insert(
            TAGS_PROPERTY.to_string(),
            tags_as_list(&tags, UPPER_CASED_TAG),
        );
    }
    put_composite_alarm_configuration(
        ctx,
        region,
        &resource.physical_resource_id,
        &Value::Object(properties),
    )
    .await
}

/// Apply a CHANGED declaration to the composite alarm the stack already has, through the same
/// `PutCompositeAlarm`: that one call "creates or updates a composite alarm", so the
/// configuration the template now declares is put under the name the alarm was created with.
pub(super) async fn update_composite_alarm(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    refuse_unapplied_composite_properties(declared, &resource.resource_type)?;
    put_composite_alarm_configuration(ctx, region, &resource.physical_resource_id, declared).await
}

/// `PutCompositeAlarm` with `properties` as the request and `name` as the one member
/// CloudFormation decides — the call behind both this type's create and its update, so the two
/// cannot send a different request for the same declaration.
///
/// The alarm's own ARN is the one CloudFormation's naming rules compute for the resource
/// (`arn:<partition>:cloudwatch:<region>:<account>:alarm:<name>`), and the put answers no
/// document at all, so nothing of the answer is recorded on the resource.
async fn put_composite_alarm_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
    properties: &Value,
) -> Result<Created, Refusal> {
    call_json_rpc(
        ctx,
        CLOUDWATCH_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{CLOUDWATCH_TARGET_PREFIX}.{PUT_COMPOSITE_ALARM}"),
        &Value::Object(composite_alarm_request(name, properties)),
    )
    .await?;
    Ok(Created::default())
}

/// The `PutCompositeAlarm` request one declaration becomes: every property the type publishes
/// that the declaration carries, under its own name and read as the TYPE that call models the
/// member as ([`composite_alarm_scalar`]), with the alarm's NAME the one member CloudFormation
/// decides — so a name the template happened to state does not travel beside the name the
/// resource was given.
fn composite_alarm_request(name: &str, properties: &Value) -> Map<String, Value> {
    let mut request = Map::new();
    for property in COMPOSITE_ALARM_PROPERTIES {
        if *property == COMPOSITE_ALARM_NAME_MEMBER {
            continue;
        }
        if let Some(value) = properties.get(*property).filter(|value| !value.is_null()) {
            request.insert(
                (*property).to_string(),
                modelled_declaration(property, value, &composite_alarm_scalar),
            );
        }
    }
    request.insert(
        COMPOSITE_ALARM_NAME_MEMBER.to_string(),
        Value::String(name.to_string()),
    );
    request
}

/// What `PutCompositeAlarm` models each member this type declares as, where that is not text.
///
/// Source: that call's own member list
/// (`smithy_query(query="operation",operation="PutCompositeAlarm")`) — `ActionsEnabled` is a
/// boolean and the two suppression periods are integers; every other member of it, the alarm
/// rule and the action lists included, is text. A tag set is a collection of TEXT whose keys
/// are the requester's, so a value in it that merely reads as a number stays the text it was
/// declared as.
fn composite_alarm_scalar(member: &str) -> ModelledScalar {
    match member {
        "ActionsEnabled" => ModelledScalar::Boolean,
        "ActionsSuppressorExtensionPeriod" | "ActionsSuppressorWaitPeriod" => {
            ModelledScalar::Number
        }
        TAGS_PROPERTY => ModelledScalar::TextThroughout,
        _ => ModelledScalar::AsWritten,
    }
}

/// Delete the composite alarm a stack created, by the name it was created under — this
/// resource's physical id. "Deletes the specified alarms", and one CloudWatch no longer holds
/// (`ResourceNotFound`, judged on the CODE and never on a message that carries the request's
/// own values) is already in the state the delete asks for.
pub(super) async fn delete_composite_alarm(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let request = serde_json::json!({ ALARM_NAMES_MEMBER: [physical_resource_id] });
    match call_json_rpc(
        ctx,
        CLOUDWATCH_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{CLOUDWATCH_TARGET_PREFIX}.{DELETE_ALARMS}"),
        &request,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == ALARM_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The composite alarm's configuration as CloudWatch reports it, for drift detection: the
/// `DescribeAlarms` entry for the name that is this resource's physical id, projected onto the
/// property names the type declares ([`REPORTED_COMPOSITE_PROPERTIES`]). An alarm that read
/// reports none of was deleted, which is what `None` reports.
pub(super) async fn describe_composite_alarm(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ ALARM_NAMES_MEMBER: [physical_resource_id] });
    let answered = call_json_rpc(
        ctx,
        CLOUDWATCH_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{CLOUDWATCH_TARGET_PREFIX}.{DESCRIBE_ALARMS}"),
        &request,
    )
    .await?;
    let Some(reported) = answered
        .get(COMPOSITE_ALARMS_MEMBER)
        .and_then(Value::as_array)
        .and_then(|alarms| {
            alarms.iter().find(|alarm| {
                alarm
                    .get(COMPOSITE_ALARM_NAME_MEMBER)
                    .and_then(Value::as_str)
                    == Some(physical_resource_id)
            })
        })
    else {
        return Ok(None);
    };
    let mut configuration = Map::new();
    for property in REPORTED_COMPOSITE_PROPERTIES {
        if let Some(value) = reported.get(*property).filter(|value| !value.is_null()) {
            configuration.insert((*property).to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(configuration)))
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps an alarm from being reported created with less than its
/// declaration asked for.
fn refuse_unapplied_composite_properties(
    properties: &Value,
    resource_type: &str,
) -> Result<(), Refusal> {
    let Value::Object(members) = properties else {
        return Ok(());
    };
    for property in members.keys() {
        if COMPOSITE_ALARM_PROPERTIES.contains(&property.as_str()) {
            continue;
        }
        return Err(Refusal {
            code: "NotImplemented".to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every property the type publishes is a member `PutCompositeAlarm` takes, and the name
    /// is the one CloudFormation decides: the request a declaration becomes carries the
    /// declared members under their own names and the resource's physical id as the alarm's
    /// name — asserted through [`composite_alarm_request`], which is the function the put
    /// itself sends.
    #[test]
    fn the_request_is_the_declaration_under_the_resources_own_name() {
        let declared = serde_json::json!({
            "AlarmName": "a-name-the-template-asked-for",
            "AlarmRule": "ALARM(child-one) OR ALARM(child-two)",
            "AlarmDescription": "the roll-up",
            "ActionsEnabled": true,
            "AlarmActions": ["arn:aws:sns:us-west-2:000000000000:paged"],
        });
        let request = composite_alarm_request("the-resources-own-name", &declared);
        assert_eq!(
            request.get(COMPOSITE_ALARM_NAME_MEMBER),
            Some(&Value::String("the-resources-own-name".to_string())),
            "case \"the declared name\": expected the alarm to be put under the name the \
             resource was given, got {request:?}"
        );
        for property in [
            "AlarmRule",
            "AlarmDescription",
            "ActionsEnabled",
            "AlarmActions",
        ] {
            assert_eq!(
                request.get(property),
                declared.get(property),
                "case {property:?}: expected the declared value to travel as written, got {:?}",
                request.get(property)
            );
        }
        // A property the declaration does not carry is not sent at all, so CloudWatch is
        // never asked to configure a member the template said nothing about.
        assert!(
            !request.contains_key("ActionsSuppressor"),
            "case \"an undeclared property\": expected it to be left out, got {request:?}"
        );
    }

    /// RULE: every member the put models as a number or a boolean is read as that type out of
    /// the TEXT a template carries it as, while text and a tag whose value reads as a number
    /// travel as written — asserted through [`composite_alarm_request`], which is the function
    /// the put itself sends.
    #[test]
    fn a_declared_scalar_reaches_the_put_as_the_type_it_models() {
        let declared = serde_json::json!({
            "AlarmRule": "ALARM(child-one) OR ALARM(child-two)",
            "ActionsEnabled": "TRUE",
            "ActionsSuppressor": "a-suppressor",
            "ActionsSuppressorWaitPeriod": "120",
            "ActionsSuppressorExtensionPeriod": "60",
            "Tags": [{ "Key": "ActionsSuppressorWaitPeriod", "Value": "120" }],
        });
        let request = composite_alarm_request("the-resources-own-name", &declared);
        let expected: &[(&str, Value)] = &[
            ("ActionsEnabled", serde_json::json!(true)),
            ("ActionsSuppressorWaitPeriod", serde_json::json!(120)),
            ("ActionsSuppressorExtensionPeriod", serde_json::json!(60)),
            ("ActionsSuppressor", serde_json::json!("a-suppressor")),
            (
                "Tags",
                serde_json::json!([{ "Key": "ActionsSuppressorWaitPeriod", "Value": "120" }]),
            ),
        ];
        for (member, wanted) in expected {
            let sent = request.get(*member);
            assert_eq!(
                sent,
                Some(wanted),
                "case {member:?}: expected the put to carry {wanted}, got {sent:?}"
            );
        }
    }

    /// A property outside the type's own set is refused rather than dropped, and every
    /// property of the set is accepted.
    #[test]
    fn a_property_the_call_does_not_carry_is_refused() {
        let accepted = serde_json::json!({ "AlarmRule": "ALARM(child)", "OKActions": [] });
        assert!(
            refuse_unapplied_composite_properties(&accepted, CLOUDWATCH_COMPOSITE_ALARM).is_ok(),
            "case \"two published properties\": expected them to be applied, got a refusal"
        );
        let refused = serde_json::json!({ "AlarmRule": "ALARM(child)", "NotAProperty": true });
        let answer = refuse_unapplied_composite_properties(&refused, CLOUDWATCH_COMPOSITE_ALARM);
        assert!(
            answer.is_err(),
            "case \"a property the type does not publish\": expected a refusal, got {answer:?}"
        );
    }
}
