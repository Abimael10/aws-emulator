// SPDX-License-Identifier: CC0-1.0

//! `AWS::IoT::ProvisioningTemplate` and `AWS::IoT::RoleAlias`: the fleet-provisioning
//! template a stack declares and the role alias a device assumes credentials through, each
//! created in AWS IoT itself.
//!
//! This is the per-type half of provisioning for two resource types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::iot_authorizer`] for the reason that module records —
//! the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the resources are really created
//!
//! Each of these resources is addressed in the owning service by the NAME it is created
//! under: `DescribeProvisioningTemplate`, `UpdateProvisioningTemplate`,
//! `CreateProvisioningClaim` and `DeleteProvisioningTemplate` take a `templateName`, and
//! `DescribeRoleAlias`, `UpdateRoleAlias` and `DeleteRoleAlias` take a `roleAlias`. So a
//! stack that reported either created while nothing created it publishes a name that
//! resolves to nothing — no device could be provisioned through that template and no
//! credential could be issued through that alias — and a SECOND stack declaring the same
//! name would be accepted where AWS IoT refuses it with
//! `ResourceAlreadyExistsException`. Creating them where they belong is what makes both
//! answers the owning service's own, the refusal of a `credentialDurationSeconds` outside
//! the range that service models included.
//!
//! ## The properties
//!
//! Each type's properties are the create's own members with an upper-case initial — the
//! template's `TemplateBody`, `ProvisioningRoleArn`, `Description`, `Enabled`,
//! `PreProvisioningHook` and `TemplateType`, the alias's `RoleArn` and
//! `CredentialDurationSeconds` — beside the `TemplateName` / `RoleAlias` the resource is
//! created UNDER, which is its physical id and what its `Ref` resolves to, and the `Tags`
//! both creates carry. A declaration that names none is created under the identifier
//! CloudFormation computed for the resource.
//!
//! A declared property an arm does not apply is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a resource reported created with less than its
//! declaration asked for is the shallow success these arms exist to avoid.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{applicable_tags, call_rest_json, percent_encoded, Created, Readiness, Refusal, Removal};
use crate::state::provisioner::tags_of;
use crate::state::StoredResource;

/// The resource types this module provisions.
pub(crate) const IOT_PROVISIONING_TEMPLATE: &str = "AWS::IoT::ProvisioningTemplate";
pub(crate) const IOT_ROLE_ALIAS: &str = "AWS::IoT::RoleAlias";

/// The SigV4 signing name of the service that owns them, which is what routes the in-process
/// call to AWS IoT's own handlers (that model's `sigv4Name`/`endpointPrefix`,
/// `smithy_query(query="service_info")`). It speaks `restJson1`, so each call is a method and
/// a URI rather than an `X-Amz-Target`.
const IOT_SIGNING_NAME: &str = "iot";

/// The error code AWS IoT answers for a template or an alias it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its reads, updates and deletes), which
/// is what says a delete has nothing left to remove and a read has nothing to report.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that states none of a required property.
const MISSING_PROPERTY: &str = "InvalidRequest";

/// Which of the two types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::IoT::ProvisioningTemplate`.
    ProvisioningTemplate,
    /// `AWS::IoT::RoleAlias`.
    RoleAlias,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        IOT_PROVISIONING_TEMPLATE => Some(Kind::ProvisioningTemplate),
        IOT_ROLE_ALIAS => Some(Kind::RoleAlias),
        _ => None,
    }
}

/// The property each type names its resource with, which is the name the resource is created
/// under rather than a member of the create's body
/// ([`crate::state::template::declared_name_property`] reads it as the physical id).
const TEMPLATE_NAME: &str = "TemplateName";
const ROLE_ALIAS_NAME: &str = "RoleAlias";

/// Every property `AWS::IoT::ProvisioningTemplate` publishes. A declared property this list
/// does not carry is one no call here applies, and is refused rather than dropped.
const TEMPLATE_PROPERTIES: &[&str] = &[
    "Description",
    "Enabled",
    "PreProvisioningHook",
    "ProvisioningRoleArn",
    "Tags",
    "TemplateBody",
    "TemplateName",
    "TemplateType",
];

/// Every property `AWS::IoT::RoleAlias` publishes.
const ROLE_ALIAS_PROPERTIES: &[&str] = &[
    "CredentialDurationSeconds",
    "RoleAlias",
    "RoleArn",
    "Tags",
];

/// The members `CreateProvisioningTemplate` takes, paired with the property that carries
/// each: the property with a lower-case initial, except the template's TYPE, which that call
/// spells `type` where the resource type publishes `TemplateType`.
///
/// Source: `CreateProvisioningTemplate`'s member list
/// (`smithy_query(query="operation",operation="CreateProvisioningTemplate")`), intersected
/// with the properties the type publishes. `templateBody` and `provisioningRoleArn` are
/// required by both.
const TEMPLATE_CREATE_MEMBERS: &[(&str, &str)] = &[
    ("Description", "description"),
    ("Enabled", "enabled"),
    ("PreProvisioningHook", "preProvisioningHook"),
    ("ProvisioningRoleArn", "provisioningRoleArn"),
    ("TemplateBody", "templateBody"),
    ("TemplateType", "type"),
];

/// The members `UpdateProvisioningTemplate` takes, which are the properties a declaration can
/// change on the template that already exists.
///
/// Source: `UpdateProvisioningTemplate`'s member list. `templateBody` and `type` are NOT
/// among them — a template's body and its kind are fixed when it is created — so a
/// declaration that changes either has no in-place path ([`update`]).
const TEMPLATE_UPDATE_MEMBERS: &[(&str, &str)] = &[
    ("Description", "description"),
    ("Enabled", "enabled"),
    ("PreProvisioningHook", "preProvisioningHook"),
    ("ProvisioningRoleArn", "provisioningRoleArn"),
];

/// The members `CreateRoleAlias` takes beside the alias's own name, paired with the property
/// that carries each — and the same members `UpdateRoleAlias` takes, which is why one list
/// serves the create and the update.
const ROLE_ALIAS_MEMBERS: &[(&str, &str)] = &[
    ("CredentialDurationSeconds", "credentialDurationSeconds"),
    ("RoleArn", "roleArn"),
];

/// The two members AWS IoT's own `Tag` structure spells a tag's key and value with
/// (`smithy_query(query="shape",shape="Tag")`).
const TAG_MEMBERS: (&str, &str) = ("Key", "Value");

/// The properties the API models as BOOLEANS, which a template may write as a quoted scalar
/// or feed through a `Ref` to a `String` parameter — every value of one arrives as text.
const FLAG_PROPERTIES: &[&str] = &["Enabled"];

/// The properties the API models as INTEGERS, which arrive the same way.
const NUMBER_PROPERTIES: &[&str] = &["CredentialDurationSeconds"];

/// The members of `ProvisioningHook`, paired with the property of the `ProvisioningHook`
/// structure the type publishes that carries each
/// (`smithy_query(query="shape",shape="ProvisioningHook")`).
const HOOK_MEMBERS: &[(&str, &str)] = &[
    ("PayloadVersion", "payloadVersion"),
    ("TargetArn", "targetArn"),
];

/// The attribute `AWS::IoT::ProvisioningTemplate` publishes through `Fn::GetAtt`
/// (`TemplateArn`), read off the member the create and the read answer it under.
const TEMPLATE_ARN_ATTRIBUTE: &str = "TemplateArn";
const TEMPLATE_ARN_MEMBER: &str = "templateArn";

/// The attribute `AWS::IoT::RoleAlias` publishes through `Fn::GetAtt` (`RoleAliasArn`), read
/// off the member the create and the read answer it under.
const ROLE_ALIAS_ARN_ATTRIBUTE: &str = "RoleAliasArn";
const ROLE_ALIAS_ARN_MEMBER: &str = "roleAliasArn";

/// The member `DescribeRoleAlias` reports the alias's record under.
const ROLE_ALIAS_DESCRIPTION: &str = "roleAliasDescription";

/// Create the resource one of these types declares, in AWS IoT itself — so a client that
/// deployed the stack can pass `!Ref <template>` straight to `CreateProvisioningClaim`, or
/// `!Ref <alias>` to `DescribeRoleAlias`, and a second stack declaring the same name is
/// refused by the service that owns the name.
///
/// `Err` is the owning service's own refusal of the create, which is what the resource fails
/// with: a name the account already holds, a role ARN or a credential duration it will not
/// take, a resource count past the account's limit.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    match kind {
        Kind::ProvisioningTemplate => create_template(ctx, region, resource, propagated).await,
        Kind::RoleAlias => create_role_alias(ctx, region, resource, propagated).await,
    }
}

/// Create the template an `AWS::IoT::ProvisioningTemplate` declares, under the name
/// CloudFormation decided for the resource.
async fn create_template(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        TEMPLATE_PROPERTIES,
    )?;
    let name = resource.physical_resource_id.clone();
    let mut request = declared_members(&resource.properties, TEMPLATE_CREATE_MEMBERS);
    // `CreateProvisioningTemplate` takes the template's name in its BODY, unlike the reads
    // and writes of one, which bind it as a URI label.
    request.insert("templateName".to_string(), Value::String(name));
    // Both are required by the call and by the type: a declaration stating neither the body
    // that describes what is provisioned nor the role it is provisioned as describes no
    // template that could be created.
    for (property, member) in [
        ("TemplateBody", "templateBody"),
        ("ProvisioningRoleArn", "provisioningRoleArn"),
    ] {
        if !request.contains_key(member) {
            return Err(missing_property(&resource.resource_type, property));
        }
    }
    insert_tags(&mut request, resource, propagated);
    let answered = call_iot(ctx, region, "POST", "/provisioning-templates", Some(&Value::Object(request))).await?;
    Ok(Created::attributes(template_attributes(&answered)))
}

/// Create the alias an `AWS::IoT::RoleAlias` declares, under the name CloudFormation decided
/// for the resource — which `CreateRoleAlias` binds as its URI label.
async fn create_role_alias(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        ROLE_ALIAS_PROPERTIES,
    )?;
    let mut request = declared_members(&resource.properties, ROLE_ALIAS_MEMBERS);
    // The role is what an alias IS a name for, and the call declares it required: a
    // declaration naming none describes no alias that could issue a credential.
    if !request.contains_key("roleArn") {
        return Err(missing_property(&resource.resource_type, "RoleArn"));
    }
    insert_tags(&mut request, resource, propagated);
    let answered = call_iot(
        ctx,
        region,
        "POST",
        &role_alias_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Created::attributes(role_alias_attributes(&answered)))
}

/// Whether the owning service reports the created resource ready.
///
/// Nothing is asked, and nothing waits: a provisioning template and a role alias each EXIST
/// from the moment their create answers — the owning service's own read resolves either, and
/// a provisioning claim can be issued through the template at once — and AWS IoT reports no
/// transient state either settles out of.
pub(super) fn ready() -> Readiness {
    Readiness::Ready
}

/// Whether the owning service has finished removing a deleted resource.
///
/// Nothing is asked: each delete takes the resource away as it answers, and AWS IoT reports
/// no state a deleted one is still being removed in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the resource the stack already has, through the owning
/// service's own update — the call that takes the resource's name, so it keeps its identity
/// and nothing is replaced.
///
/// `Ok(None)` is a change that call cannot make: a template's `TemplateBody` and
/// `TemplateType` are not members of `UpdateProvisioningTemplate`, so a deployment that
/// changes either reports the change as not applied rather than as a success that dropped the
/// caller's change.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    match kind {
        Kind::ProvisioningTemplate => update_template(ctx, region, resource, declared).await,
        Kind::RoleAlias => update_role_alias(ctx, region, resource, declared).await,
    }
}

/// `UpdateProvisioningTemplate` over the members that call takes.
async fn update_template(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    // The DECLARED document, not the record's: by the time this is reached the resource
    // carries the properties it was CREATED with, so a property the new declaration
    // introduces is only in `declared`.
    refuse_unapplied_properties(declared, &resource.resource_type, TEMPLATE_PROPERTIES)?;
    for fixed in ["TemplateBody", "TemplateType"] {
        if declared.get(fixed) != resource.properties.get(fixed) {
            return Ok(None);
        }
    }
    let request = declared_members(declared, TEMPLATE_UPDATE_MEMBERS);
    call_iot(
        ctx,
        region,
        "PATCH",
        &template_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    // `UpdateProvisioningTemplate` answers no members at all, so the attribute the type
    // publishes is read back off the template that now exists rather than invented here.
    let described = call_iot(
        ctx,
        region,
        "GET",
        &template_uri(&resource.physical_resource_id),
        None,
    )
    .await?;
    Ok(Some(Created::attributes(template_attributes(&described))))
}

/// `UpdateRoleAlias` over the members that call takes.
async fn update_role_alias(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, &resource.resource_type, ROLE_ALIAS_PROPERTIES)?;
    let request = declared_members(declared, ROLE_ALIAS_MEMBERS);
    let answered = call_iot(
        ctx,
        region,
        "PUT",
        &role_alias_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Some(Created::attributes(role_alias_attributes(&answered))))
}

/// Delete the resource a stack created, addressed by the name it was created under.
///
/// A resource the owning service no longer holds is already in the state the delete asks for,
/// judged on the CODE and never on a message.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let uri = match kind {
        Kind::ProvisioningTemplate => template_uri(&resource.physical_resource_id),
        Kind::RoleAlias => role_alias_uri(&resource.physical_resource_id),
    };
    match call_iot(ctx, region, "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The resource's configuration as the owning service reports it, for drift detection.
///
/// Each read answers the very members the type declares its properties with, so the answer
/// maps onto the property vocabulary one for one. A resource the owning service no longer
/// holds was deleted ([`NOT_FOUND`]).
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let uri = match kind {
        Kind::ProvisioningTemplate => template_uri(&resource.physical_resource_id),
        Kind::RoleAlias => role_alias_uri(&resource.physical_resource_id),
    };
    let answered = match call_iot(ctx, region, "GET", &uri, None).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let (described, members, name_property, name_member) = match kind {
        Kind::ProvisioningTemplate => (
            &answered,
            TEMPLATE_CREATE_MEMBERS,
            TEMPLATE_NAME,
            "templateName",
        ),
        // The alias's record is reported one level in, under the member that carries it.
        Kind::RoleAlias => {
            let Some(described) = answered.get(ROLE_ALIAS_DESCRIPTION) else {
                return Ok(None);
            };
            (described, ROLE_ALIAS_MEMBERS, ROLE_ALIAS_NAME, "roleAlias")
        }
    };
    let mut configuration = Map::new();
    for (property, member) in members
        .iter()
        .chain([&(name_property, name_member)])
    {
        if let Some(value) = described.get(*member) {
            configuration.insert((*property).to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(configuration)))
}

/// The URI the per-template operations are bound to (`/provisioning-templates/{templateName}`,
/// that model's own `http` binding), with the name written as ONE path segment so a name
/// outside the modelled pattern is refused by that operation's validation instead of
/// addressing a different one.
fn template_uri(name: &str) -> String {
    format!("/provisioning-templates/{}", percent_encoded(name))
}

/// The URI the per-alias operations are bound to (`/role-aliases/{roleAlias}`), with the name
/// written as ONE path segment for the same reason.
fn role_alias_uri(name: &str) -> String {
    format!("/role-aliases/{}", percent_encoded(name))
}

/// The attributes an answer reports for a provisioning template: the ARN that service minted,
/// which is the type's one published `Fn::GetAtt` attribute.
fn template_attributes(answered: &Value) -> BTreeMap<String, String> {
    reported_arn(answered, TEMPLATE_ARN_MEMBER, TEMPLATE_ARN_ATTRIBUTE)
}

/// The attributes an answer reports for a role alias, read the same way.
fn role_alias_attributes(answered: &Value) -> BTreeMap<String, String> {
    reported_arn(answered, ROLE_ALIAS_ARN_MEMBER, ROLE_ALIAS_ARN_ATTRIBUTE)
}

/// The ARN `member` carries, as the published attribute `attribute` — and no attribute at all
/// for an answer that carries none, rather than an empty one.
fn reported_arn(answered: &Value, member: &str, attribute: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    if let Some(arn) = answered
        .get(member)
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
    {
        attributes.insert(attribute.to_string(), arn.to_string());
    }
    attributes
}

/// The refusal a declaration that states none of a required property earns, naming it.
fn missing_property(resource_type: &str, property: &str) -> Refusal {
    Refusal {
        code: MISSING_PROPERTY.to_string(),
        message: format!("{resource_type} declares no {property}"),
    }
}

/// Add the tags the resource carries to `request`, as the two creates' own `tags` list.
///
/// The resource carries the tags its declaration asks for plus the ones CloudFormation gives
/// it — less the keys in AWS's reserved namespace, which this service documents as its own
/// ("Do not use the `aws:` prefix ... it is reserved for AWS use") and which no public call
/// of it writes.
fn insert_tags(
    request: &mut Map<String, Value>,
    resource: &StoredResource,
    propagated: &[(String, String)],
) {
    let tags = applicable_tags(&tags_of(&resource.properties, propagated));
    if tags.is_empty() {
        return;
    }
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
}

/// The members of `members` a declaration states, as the request carries them: the property
/// and the member are the same name up to the initial's case (except the template's
/// `TemplateType`, which the call spells `type`), so every value goes through as the template
/// wrote it — except a member the API models as a BOOLEAN or an INTEGER, read through
/// [`declared_flag`] and [`declared_number`], and the provisioning HOOK, whose own members
/// are spelled the same way one level in.
fn declared_members(properties: &Value, members: &[(&str, &str)]) -> Map<String, Value> {
    let mut request = Map::new();
    for (property, member) in members {
        let Some(value) = properties.get(*property).filter(|value| !value.is_null()) else {
            continue;
        };
        let carried = if FLAG_PROPERTIES.contains(property) {
            declared_flag(value).map_or_else(|| value.clone(), Value::Bool)
        } else if NUMBER_PROPERTIES.contains(property) {
            declared_number(value).unwrap_or_else(|| value.clone())
        } else if *member == "preProvisioningHook" {
            declared_hook(value)
        } else {
            value.clone()
        };
        request.insert((*member).to_string(), carried);
    }
    request
}

/// The `preProvisioningHook` a declaration states, as `ProvisioningHook`'s own members carry
/// it. A value that is no object at all is left exactly as written, so the owning service's
/// own validation is what judges it.
fn declared_hook(value: &Value) -> Value {
    let Value::Object(declared) = value else {
        return value.clone();
    };
    let mut hook = Map::new();
    for (property, member) in HOOK_MEMBERS {
        if let Some(carried) = declared.get(*property).filter(|value| !value.is_null()) {
            hook.insert((*member).to_string(), carried.clone());
        }
    }
    Value::Object(hook)
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

/// A declared integer in either spelling a template gives one: JSON's number, or the digits a
/// template spells one as — a value a `Ref` to a `String` parameter always arrives as. `None`
/// is a value that is no integer, left as written for the owning service to judge, which is
/// what refuses a duration outside the range it models.
fn declared_number(value: &Value) -> Option<Value> {
    match value {
        Value::Number(_) => Some(value.clone()),
        Value::String(text) => text.trim().parse::<i64>().ok().map(Value::from),
        _ => None,
    }
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a resource from being reported as created — or as updated —
/// with less than its declaration asked for.
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

    /// The declared properties reach `CreateProvisioningTemplate` under that call's own member
    /// names — the template's kind under `type`, a flag written as text as the boolean the API
    /// models, and the hook's own members one level in.
    #[test]
    fn a_templates_properties_are_carried_under_the_api_member_names() {
        let declared = serde_json::json!({
            "TemplateName": "fleet-template",
            "TemplateBody": "{\"Resources\":{}}",
            "ProvisioningRoleArn": "arn:aws:iam::000000000000:role/provision",
            "Description": "a fleet template",
            "Enabled": "true",
            "TemplateType": "FLEET_PROVISIONING",
            "PreProvisioningHook": {
                "PayloadVersion": "2020-04-01",
                "TargetArn": "arn:aws:lambda:us-east-1:000000000000:function:hook"
            }
        });
        let carried = Value::Object(declared_members(&declared, TEMPLATE_CREATE_MEMBERS));
        let expected = serde_json::json!({
            "templateBody": "{\"Resources\":{}}",
            "provisioningRoleArn": "arn:aws:iam::000000000000:role/provision",
            "description": "a fleet template",
            "enabled": true,
            "type": "FLEET_PROVISIONING",
            "preProvisioningHook": {
                "payloadVersion": "2020-04-01",
                "targetArn": "arn:aws:lambda:us-east-1:000000000000:function:hook"
            }
        });
        assert_eq!(
            carried, expected,
            "case \"a full declaration\": expected {expected}, got {carried}"
        );
    }

    /// An alias's properties reach `CreateRoleAlias` the same way, with the duration written
    /// as text carried as the integer the API models — and a value that is no integer left as
    /// written, for the owning service to refuse.
    #[test]
    fn an_aliases_duration_is_carried_as_the_integer_the_api_models() {
        let cases: &[(&str, Value, Value)] = &[
            (
                "a duration written as a number",
                serde_json::json!({"RoleArn": "arn:aws:iam::000000000000:role/r", "CredentialDurationSeconds": 3600}),
                serde_json::json!({"roleArn": "arn:aws:iam::000000000000:role/r", "credentialDurationSeconds": 3600}),
            ),
            (
                "a duration written as text",
                serde_json::json!({"RoleArn": "arn:aws:iam::000000000000:role/r", "CredentialDurationSeconds": "900"}),
                serde_json::json!({"roleArn": "arn:aws:iam::000000000000:role/r", "credentialDurationSeconds": 900}),
            ),
            (
                "a duration outside the modelled range",
                serde_json::json!({"RoleArn": "arn:aws:iam::000000000000:role/r", "CredentialDurationSeconds": 60}),
                serde_json::json!({"roleArn": "arn:aws:iam::000000000000:role/r", "credentialDurationSeconds": 60}),
            ),
            (
                "a duration that is no integer",
                serde_json::json!({"RoleArn": "arn:aws:iam::000000000000:role/r", "CredentialDurationSeconds": "an hour"}),
                serde_json::json!({"roleArn": "arn:aws:iam::000000000000:role/r", "credentialDurationSeconds": "an hour"}),
            ),
        ];
        for (case, declared, expected) in cases {
            let carried = Value::Object(declared_members(declared, ROLE_ALIAS_MEMBERS));
            assert_eq!(
                carried, *expected,
                "case {case:?}: expected {expected}, got {carried}"
            );
        }
    }

    /// Each URI is the model's own binding with the name as ONE path segment, so a name
    /// carrying a character that is structure in a URI cannot address another operation.
    #[test]
    fn the_uris_carry_the_name_as_one_segment() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "an ordinary name",
                "fleet-template",
                "/provisioning-templates/fleet-template",
                "/role-aliases/fleet-template",
            ),
            (
                "a name with a slash",
                "a/b",
                "/provisioning-templates/a%2Fb",
                "/role-aliases/a%2Fb",
            ),
            (
                "a name with an at sign",
                "a@b",
                "/provisioning-templates/a%40b",
                "/role-aliases/a%40b",
            ),
        ];
        for (case, name, template, alias) in cases {
            assert_eq!(
                template_uri(name),
                *template,
                "case {case:?}: expected the template URI {template:?}, got {:?}",
                template_uri(name)
            );
            assert_eq!(
                role_alias_uri(name),
                *alias,
                "case {case:?}: expected the alias URI {alias:?}, got {:?}",
                role_alias_uri(name)
            );
        }
    }

    /// The ARN the owning service minted is reported as each type's published attribute, and
    /// an answer carrying none reports no attribute rather than an empty one.
    #[test]
    fn the_minted_arns_are_the_published_attributes() {
        let template = serde_json::json!({
            "templateName": "fleet-template",
            "templateArn": "arn:aws:iot:us-east-1:000000000000:provisioningtemplate/fleet-template"
        });
        assert_eq!(
            template_attributes(&template)
                .get(TEMPLATE_ARN_ATTRIBUTE)
                .map(String::as_str),
            Some("arn:aws:iot:us-east-1:000000000000:provisioningtemplate/fleet-template"),
            "case \"an answered template ARN\": expected it to be the TemplateArn attribute"
        );
        let alias = serde_json::json!({
            "roleAlias": "device-alias",
            "roleAliasArn": "arn:aws:iot:us-east-1:000000000000:rolealias/device-alias"
        });
        assert_eq!(
            role_alias_attributes(&alias)
                .get(ROLE_ALIAS_ARN_ATTRIBUTE)
                .map(String::as_str),
            Some("arn:aws:iot:us-east-1:000000000000:rolealias/device-alias"),
            "case \"an answered alias ARN\": expected it to be the RoleAliasArn attribute"
        );
        for (case, silent) in [
            ("a template answer carrying no ARN", &template),
            ("an alias answer carrying no ARN", &alias),
        ] {
            let other = Value::Object(
                silent
                    .as_object()
                    .expect("the fixture is an object")
                    .iter()
                    .filter(|(member, _)| !member.ends_with("Arn"))
                    .map(|(member, value)| (member.clone(), value.clone()))
                    .collect(),
            );
            assert!(
                template_attributes(&other).is_empty() && role_alias_attributes(&other).is_empty(),
                "case {case:?}: expected no attribute at all, got {:?} and {:?}",
                template_attributes(&other),
                role_alias_attributes(&other)
            );
        }
    }

    /// A property no call here applies is refused, naming it; every property each type
    /// publishes is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        let template = serde_json::json!({
            "TemplateName": "fleet-template",
            "TemplateBody": "{}",
            "ProvisioningRoleArn": "arn:aws:iam::000000000000:role/provision",
            "Description": "d",
            "Enabled": true,
            "TemplateType": "FLEET_PROVISIONING",
            "PreProvisioningHook": {},
            "Tags": []
        });
        assert!(
            refuse_unapplied_properties(&template, IOT_PROVISIONING_TEMPLATE, TEMPLATE_PROPERTIES)
                .is_ok(),
            "case \"every published template property\": expected it to be applied"
        );
        let alias = serde_json::json!({
            "RoleAlias": "device-alias",
            "RoleArn": "arn:aws:iam::000000000000:role/r",
            "CredentialDurationSeconds": 3600,
            "Tags": []
        });
        assert!(
            refuse_unapplied_properties(&alias, IOT_ROLE_ALIAS, ROLE_ALIAS_PROPERTIES).is_ok(),
            "case \"every published alias property\": expected it to be applied"
        );
        let unknown = serde_json::json!({"RoleAlias": "device-alias", "Unheard": 1});
        let refusal = refuse_unapplied_properties(&unknown, IOT_ROLE_ALIAS, ROLE_ALIAS_PROPERTIES)
            .expect_err("case \"a property no call carries\": expected a refusal");
        assert!(
            refusal.message.contains("Unheard"),
            "case \"a property no call carries\": expected the refusal to name it, got {:?}",
            refusal.message
        );
    }

    /// The kind is read off the resource type, and no other type is owned here.
    #[test]
    fn each_type_resolves_to_its_own_kind() {
        let cases: &[(&str, Option<Kind>)] = &[
            (IOT_PROVISIONING_TEMPLATE, Some(Kind::ProvisioningTemplate)),
            (IOT_ROLE_ALIAS, Some(Kind::RoleAlias)),
            ("AWS::IoT::Authorizer", None),
            ("AWS::IoT::Thing", None),
        ];
        for (resource_type, expected) in cases {
            let resolved = kind_of(resource_type);
            assert_eq!(
                resolved, *expected,
                "case {resource_type:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }
}
