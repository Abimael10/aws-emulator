// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of AWS IoT Core Device Advisor's own control plane: the
//! test suite an `AWS::IoTCoreDeviceAdvisor::SuiteDefinition` declares. One file of
//! [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! A suite definition is a configuration record rather than a named resource:
//! `CreateSuiteDefinition` takes the suite's whole configuration — the name, the device
//! permission role, the devices under test and the root group of tests — takes no identifier
//! at all, and answers the `suiteDefinitionId` that service drew (a `UUID`) with the
//! `suitedefinition/<id>` ARN built around it. That id is the value this type's `Ref` returns
//! and the handle `GetSuiteDefinition`, `UpdateSuiteDefinition` and `DeleteSuiteDefinition`
//! each address one by, so it becomes the resource's physical id, replacing the stand-in the
//! naming rules computed.
//!
//! The type's one configuration property IS that call's own `suiteDefinitionConfiguration`
//! member once each initial is lower-cased ([`camel_cased`], which walks the nested `Devices`
//! the same way: a device's `ThingArn` is that member's `thingArn`). The other property is
//! `Tags`, which the type publishes as the usual `[{Key, Value}]` list while every call of
//! that service takes a tag MAP ([`tags_as_map`]).
//!
//! The VERSION label the suite's live configuration is kept under is that service's own and
//! the create does not report it, so it is read off that service's own read of the suite the
//! create just made — exactly as an `AWS::Braket::SpendingLimit`'s `CreatedAt` is
//! ([`super::braket_spending_limits`]).

use super::*;

/// AWS IoT Core Device Advisor's signing name, from its model (`sigv4Name: iotdeviceadvisor`),
/// which speaks `restJson1` — so its calls ride on [`call_rest_json_named`], each on the URI
/// its own operation's `@http` binding declares, naming the operation so a refusal is reported
/// the way that service's own SDK raises it.
pub(super) const IOT_DEVICE_ADVISOR_SIGNING_NAME: &str = "iotdeviceadvisor";

/// The collection `CreateSuiteDefinition` is bound to (`POST /suiteDefinitions`). ONE suite is
/// addressed under it by the id that service minted (`GET`/`PATCH`/`DELETE
/// /suiteDefinitions/{suiteDefinitionId}`).
const SUITE_DEFINITIONS_URI: &str = "/suiteDefinitions";

/// The collection a suite's TAGS are addressed under, by the suite's own ARN
/// (`/tags/{resourceArn}`): `ListTagsForResource` reads them, `TagResource` writes them and
/// `UntagResource` drops them.
const TAGS_URI: &str = "/tags";

/// The operations this module calls, as that service's own model names them.
const CREATE_SUITE_DEFINITION: &str = "CreateSuiteDefinition";
const GET_SUITE_DEFINITION: &str = "GetSuiteDefinition";
const UPDATE_SUITE_DEFINITION: &str = "UpdateSuiteDefinition";
const DELETE_SUITE_DEFINITION: &str = "DeleteSuiteDefinition";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code that service answers for a suite definition it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its read and of its tagging calls).
const IOT_DEVICE_ADVISOR_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The property the suite's configuration is declared in, and the member both
/// `CreateSuiteDefinition` and `UpdateSuiteDefinition` take it as — the same document on both
/// calls, so one declaration serves this type's create and its in-place update alike.
const CONFIGURATION_PROPERTY: &str = "SuiteDefinitionConfiguration";
const CONFIGURATION_MEMBER: &str = "suiteDefinitionConfiguration";

/// The member the tag set travels as on the create and on `TagResource` alike: a `TagMap`,
/// which is the one shape every tag member of that service takes.
const TAGS_MEMBER: &str = "tags";

/// Every property of the type this arm carries onto the owning API, so a declaration stating
/// one outside the set is REFUSED rather than reported as created without it
/// ([`refuse_unmapped_properties`]): the suite's configuration and its tags.
const MAPPED_PROPERTIES: &[&str] = &[CONFIGURATION_PROPERTY, "Tags"];

/// The members that service reports a suite's identity and version in.
const SUITE_DEFINITION_ID_MEMBER: &str = "suiteDefinitionId";
const SUITE_DEFINITION_ARN_MEMBER: &str = "suiteDefinitionArn";
const SUITE_DEFINITION_VERSION_MEMBER: &str = "suiteDefinitionVersion";

/// The `(attribute, member)` pairs this type's published return values are read off that
/// service's own report of a suite by — the one table both the create and the update hand to the
/// shared reading ([`reported_attributes`]).
const PUBLISHED_ATTRIBUTES: &[(&str, &str)] = &[
    (
        template::IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION_ARN_ATTRIBUTE,
        SUITE_DEFINITION_ARN_MEMBER,
    ),
    (
        template::IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION_ID_ATTRIBUTE,
        SUITE_DEFINITION_ID_MEMBER,
    ),
    (
        template::IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION_VERSION_ATTRIBUTE,
        SUITE_DEFINITION_VERSION_MEMBER,
    ),
];

/// The member its read answers the suite's whole configuration in, which is where a drift read
/// finds the values the declaration stated.
const CONFIGURATION_ANSWER_MEMBER: &str = "suiteDefinitionConfiguration";

/// The members of that reported configuration under the properties the type declares them as:
/// the suite's name, the role Device Advisor assumes for the devices, the root group of tests
/// and whether the suite is intended for qualification.
///
/// Its `devices` is NOT among them: that service reports the device list as the shape its own
/// model spells in lower camel case, so comparing it against the declaration would need the
/// reverse of the re-casing the create does, and a mapping this module would have to invent is
/// worse than no evidence — a property the actual document does not carry is not evidence of
/// anything ([`drift`]).
const REPORTED_CONFIGURATION: &[(&str, &str)] = &[
    ("DevicePermissionRoleArn", "devicePermissionRoleArn"),
    ("IntendedForQualification", "intendedForQualification"),
    ("RootGroup", "rootGroup"),
    ("SuiteDefinitionName", "suiteDefinitionName"),
];

/// Create the test suite an `AWS::IoTCoreDeviceAdvisor::SuiteDefinition` declares, so a client
/// that deployed the stack reads the suite back through that service's own
/// `GetSuiteDefinition` for the id the stack published, finds it in `ListSuiteDefinitions`, and
/// can really start a suite run against it.
///
/// The suite is recorded in the service that OWNS it, so everything the declaration states is
/// resolved by Device Advisor exactly as it is for a client's own call: a configuration it
/// refuses — a root group a non-qualification suite may not have, a device permission role that
/// is no IAM ARN, a device under test naming no device — is that service's refusal, which fails
/// the resource and rolls the stack back rather than publishing an id for a suite nothing
/// holds.
///
/// The identity is that service's: the `suiteDefinitionId` it drew becomes this resource's
/// physical id — the primary identifier of the type, and the handle every suite-definition API
/// takes — and it is reported back beside the ARN that service built around it and the version
/// label it keeps the live configuration under.
pub(super) async fn create_suite_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, MAPPED_PROPERTIES)?;
    let mut request = Map::new();
    if let Some(configuration) = resource.properties.get(CONFIGURATION_PROPERTY) {
        request.insert(
            CONFIGURATION_MEMBER.to_string(),
            camel_cased(configuration, 0)?,
        );
    }
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        IOT_DEVICE_ADVISOR_SIGNING_NAME,
        region,
        CREATE_SUITE_DEFINITION,
        "POST",
        SUITE_DEFINITIONS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let suite_definition_id = reported_text(&answered, SUITE_DEFINITION_ID_MEMBER)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Refusal {
            // There would be no handle a client could call that service with, and nothing for
            // the stack to publish, so this is a failed create rather than a created resource.
            code: "InternalFailure".to_string(),
            message: "the AWS IoT Core Device Advisor control plane answered a created suite \
                      definition without an id"
                .to_string(),
        })?;
    let mut attributes = reported_suite_attributes(ctx, region, &suite_definition_id).await?;
    // The ARN the create itself answered, which is the handle every tagging call takes: it fills
    // the attribute in only where the read above carried none, since both report the same ARN.
    if let Some(arn) = reported_text(&answered, SUITE_DEFINITION_ARN_MEMBER) {
        attributes
            .entry(template::IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION_ARN_ATTRIBUTE.to_string())
            .or_insert(arn);
    }
    attributes.insert(
        template::IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION_ID_ATTRIBUTE.to_string(),
        suite_definition_id.clone(),
    );
    Ok(Created {
        physical_resource_id: Some(suite_definition_id),
        attributes,
    })
}

/// Apply a changed declaration to the suite the stack created, through
/// `UpdateSuiteDefinition` — the one call that reconfigures a suite that exists.
///
/// That call takes the suite's id and its WHOLE configuration, which is the same document the
/// create carried, so every property of the configuration reaches the suite in place and none
/// of them replaces the resource. The tags travel separately ([`retag_suite_definition`]), and a
/// declaration stating a property no arm carries is refused here exactly as the create refuses
/// one ([`MAPPED_PROPERTIES`]) rather than reported as applied.
pub(super) async fn update_suite_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    // The same check the create makes, against the NEW declaration: a property no arm carries
    // onto the owning API would otherwise be reported as applied while being dropped.
    if let Some(property) = declared
        .as_object()
        .and_then(|properties| unmapped_property(properties, MAPPED_PROPERTIES))
    {
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    let mut request = Map::new();
    if let Some(configuration) = declared.get(CONFIGURATION_PROPERTY) {
        request.insert(
            CONFIGURATION_MEMBER.to_string(),
            camel_cased(configuration, 0)?,
        );
    }
    let answered = call_rest_json_named(
        ctx,
        IOT_DEVICE_ADVISOR_SIGNING_NAME,
        region,
        UPDATE_SUITE_DEFINITION,
        "PATCH",
        &suite_definition_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    // What the resource now publishes is that service's own: the version label the update
    // allocated, read off its report of the suite, with the ARN and the id it answered.
    let mut attributes =
        reported_suite_attributes(ctx, region, &resource.physical_resource_id).await?;
    if let Some(version) = reported_text(&answered, SUITE_DEFINITION_VERSION_MEMBER) {
        attributes.insert(
            template::IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION_VERSION_ATTRIBUTE.to_string(),
            version,
        );
    }
    attributes.insert(
        template::IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION_ID_ATTRIBUTE.to_string(),
        resource.physical_resource_id.clone(),
    );
    Ok(Created::attributes(attributes))
}

/// Bring the tags of the suite a resource record names to the ones this deployment gives it,
/// through that service's own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the CREATE applied — so a key outside it is one this deployment
/// no longer gives the resource and is removed, and one in it with another value is re-valued.
/// Where the two already agree nothing is called at all, which is what makes this safe to ask
/// on every deployment; a suite that service no longer holds has no tags to reconcile.
///
/// Those calls address a suite by its ARN, which that service minted rather than something
/// rebuilt here — so it is read off the suite itself.
pub(super) async fn retag_suite_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let Some(arn) = read_suite_definition(ctx, region, &resource.physical_resource_id)
        .await?
        .as_ref()
        .and_then(|suite| reported_text(suite, SUITE_DEFINITION_ARN_MEMBER))
    else {
        return Ok(());
    };
    let uri = format!("{TAGS_URI}/{}", percent_encoded(&arn));
    let answered = match call_rest_json_named(
        ctx,
        IOT_DEVICE_ADVISOR_SIGNING_NAME,
        region,
        LIST_TAGS_FOR_RESOURCE,
        "GET",
        &uri,
        None,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == IOT_DEVICE_ADVISOR_RESOURCE_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    let held = reported_tags(&answered);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        call_rest_json_named(
            ctx,
            IOT_DEVICE_ADVISOR_SIGNING_NAME,
            region,
            TAG_RESOURCE,
            "POST",
            &uri,
            Some(&Value::Object(
                [(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&added)))]
                    .into_iter()
                    .collect(),
            )),
        )
        .await?;
    }
    if !removed.is_empty() {
        // `tagKeys` is an `@httpQuery` LIST, which travels as the parameter repeated once per
        // key rather than as one joined value.
        let keys = removed
            .iter()
            .map(|key| format!("tagKeys={}", percent_encoded(key)))
            .collect::<Vec<String>>()
            .join("&");
        call_rest_json_named(
            ctx,
            IOT_DEVICE_ADVISOR_SIGNING_NAME,
            region,
            UNTAG_RESOURCE,
            "DELETE",
            &format!("{uri}?{keys}"),
            None,
        )
        .await?;
    }
    Ok(())
}

/// Delete the test suite a stack created, addressed by the id that service minted for it —
/// this resource's physical id.
///
/// That operation models no not-found error at all: a suite it no longer holds is already in
/// the state the delete asks for, and it answers the delete as the no-op it is. A refusal with
/// the not-found CODE is read the same way, and never a message that carries the request's own
/// identifier.
pub(super) async fn delete_suite_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        IOT_DEVICE_ADVISOR_SIGNING_NAME,
        region,
        DELETE_SUITE_DEFINITION,
        "DELETE",
        &suite_definition_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == IOT_DEVICE_ADVISOR_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The suite's configuration as that service reports it, for drift detection: the members of
/// its own read's `suiteDefinitionConfiguration`, under the properties the type declares them
/// as ([`REPORTED_CONFIGURATION`]), nested back under the one property that carries them. A
/// suite it no longer holds was deleted, which is what `None` reports.
pub(super) async fn describe_suite_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(suite) = read_suite_definition(ctx, region, physical_resource_id).await? else {
        return Ok(None);
    };
    let Some(configuration) = suite.get(CONFIGURATION_ANSWER_MEMBER) else {
        return Ok(Some(Value::Object(Map::new())));
    };
    Ok(Some(Value::Object(
        [(
            CONFIGURATION_PROPERTY.to_string(),
            reported_as_properties(configuration, REPORTED_CONFIGURATION),
        )]
        .into_iter()
        .collect(),
    )))
}

/// The attributes the suite at `suite_definition_id` publishes, read off that service's own
/// report of it: the ARN it built, the id it minted and the version label it keeps the live
/// configuration under.
///
/// The reading itself is the shared one ([`reported_attributes`]), so a member that report does
/// not carry, or carries empty, contributes no attribute and the resource keeps the stand-in the
/// naming rules computed rather than an empty value. A suite that service reports nothing about
/// at all contributes none.
async fn reported_suite_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    suite_definition_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let Some(suite) = read_suite_definition(ctx, region, suite_definition_id).await? else {
        return Ok(BTreeMap::new());
    };
    Ok(reported_attributes(&suite, PUBLISHED_ATTRIBUTES))
}

/// The suite record that service holds under `suite_definition_id`, or `None` when it holds
/// none (`ResourceNotFoundException`, judged on the CODE and never on a message).
async fn read_suite_definition(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    suite_definition_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        IOT_DEVICE_ADVISOR_SIGNING_NAME,
        region,
        GET_SUITE_DEFINITION,
        "GET",
        &suite_definition_uri(suite_definition_id),
        None,
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == IOT_DEVICE_ADVISOR_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The tags one `ListTagsForResource` answer carries, as the ordered pairs the reconciliation
/// compares: that service answers them as the `TagMap` under its own lower-cased `tags` member,
/// which is neither spelling a declaration's `Tags` property uses — so they go through the shared
/// reading of that MAP shape rather than through [`declared_tags`].
fn reported_tags(answered: &Value) -> Vec<(String, String)> {
    reported_tag_map(answered, TAGS_MEMBER)
}

/// The URI ONE suite definition is addressed at: the collection and the `suiteDefinitionId`
/// `@httpLabel` escaped as a single path segment ([`percent_encoded`]), so a value outside that
/// member's own bounds is refused by that service's own validation rather than addressing
/// another path.
fn suite_definition_uri(suite_definition_id: &str) -> String {
    format!(
        "{SUITE_DEFINITIONS_URI}/{}",
        percent_encoded(suite_definition_id)
    )
}
