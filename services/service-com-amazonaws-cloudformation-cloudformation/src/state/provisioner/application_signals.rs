// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of CloudWatch Application Signals' own control plane: the
//! three resources a template declares there — the SERVICE LEVEL OBJECTIVE an
//! `AWS::ApplicationSignals::ServiceLevelObjective` declares, the account's GROUPING
//! CONFIGURATION an `AWS::ApplicationSignals::GroupingConfiguration` declares, and the dynamic
//! INSTRUMENTATION POINT an `AWS::ApplicationSignals::InstrumentationConfig` declares. One file
//! of [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! ## Why each is really created
//!
//! An objective, a grouping configuration and an instrumentation point are all read back
//! through Application Signals' own API: a client that deployed the stack lists its SLOs,
//! reads the account's grouping attribute definitions, and has the SDKs of its instrumented
//! services fetch the instrumentation configurations of a service and environment. A stack
//! that reported one created while nothing created it publishes an ARN that resolves to
//! nothing, and the instrumentation the template asked for never reaches a single SDK.
//!
//! ## What each declaration is
//!
//! * the OBJECTIVE's properties are `CreateServiceLevelObjective`'s own members but for the
//!   two indicators: the type publishes the SLI structures under the names a READ answers them
//!   in (`Sli`, whose own metric is `SliMetric`), while the create and the update take the
//!   CONFIG shapes (`SliConfig`, whose metric is `SliMetricConfig`) — the same members under
//!   those names, which is the one rename [`with_renamed_member`] makes. Its `ExclusionWindows` are
//!   no member of either call: exclusion windows are added to an objective that exists, by the
//!   one call that writes them ([`BATCH_UPDATE_EXCLUSION_WINDOWS`]);
//! * the GROUPING CONFIGURATION's one property IS `PutGroupingConfiguration`'s own member, and
//!   that call is documented "Creates or updates the grouping configuration for this account"
//!   — so it is this type's update as well as its create, and the account holds one
//!   configuration however many times it is put;
//! * the INSTRUMENTATION POINT's properties ARE `CreateInstrumentationConfiguration`'s own
//!   members, spelled identically on both sides, with the one instant a declaration can state
//!   (`ExpiresAt`) converted from the template's ISO-8601 text to the seconds that member
//!   carries on the wire ([`epoch_seconds`]). A configuration is "immutable after creation"
//!   and that service publishes no call that changes one, so this type has no update arm at
//!   all: every property replaces the resource
//!   ([`super::template::replacement_properties`]), and the replacement deletes first, because
//!   one location holds at most one configuration.
//!
//! A property outside each type's mapped list is REFUSED rather than dropped: a resource
//! reported as created with less than its declaration asked for is not the resource the
//! template described.

use super::*;

/// CloudWatch Application Signals' signing name, from its model (`sigv4Name:
/// application-signals`), which speaks `restJson1` — so its calls ride on
/// [`call_rest_json_named`], each on the URI its own operation's `@http` binding declares,
/// naming the operation so a refusal is reported the way that service's own SDK raises it.
pub(super) const APPLICATION_SIGNALS_SIGNING_NAME: &str = "application-signals";

/// The error code Application Signals answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of the SLO's read, update and delete and of
/// an instrumentation configuration's read and delete).
const APPLICATION_SIGNALS_NOT_FOUND: &str = "ResourceNotFoundException";

// ---------------------------------------------------------------------------
// AWS::ApplicationSignals::ServiceLevelObjective
// ---------------------------------------------------------------------------

/// The collection Application Signals binds its objectives to, as that model's `@http` traits
/// declare: `POST /slo` creates one, and `GET`/`PATCH`/`DELETE /slo/{Id}` read, change and
/// remove it. The exclusion windows of one are written through a collection of their own
/// (`PATCH /exclusion-windows`).
const SLO_URI: &str = "/slo";
const EXCLUSION_WINDOWS_URI: &str = "/exclusion-windows";

/// The operations this module calls on an objective, as Application Signals' own model names
/// them.
const CREATE_SLO: &str = "CreateServiceLevelObjective";
const GET_SLO: &str = "GetServiceLevelObjective";
const UPDATE_SLO: &str = "UpdateServiceLevelObjective";
const DELETE_SLO: &str = "DeleteServiceLevelObjective";
const BATCH_UPDATE_EXCLUSION_WINDOWS: &str = "BatchUpdateExclusionWindows";

/// The member every one of those operations answers the objective's record in.
const SLO_MEMBER: &str = "Slo";

/// The properties this arm maps onto the objective's create and update, and so the ones a
/// declaration may carry: every property of the type whose value this arm really applies.
///
/// Source: the type's own entry in the "AWS resource and property types reference" read beside
/// `CreateServiceLevelObjective`'s own members (`smithy_query(query="operation",
/// operation="CreateServiceLevelObjective")`).
const SLO_MAPPED_PROPERTIES: &[&str] = &[
    "AutoInvestigationEnabled",
    "BurnRateConfigurations",
    "Description",
    "ExclusionWindows",
    "Goal",
    "Name",
    "RequestBasedSli",
    "Sli",
    "Tags",
];

/// The properties the objective's two calls take under the SAME member name: the description,
/// the goal, the burn-rate look-back windows and whether the agent investigates a breach.
const SLO_COPIED_MEMBERS: &[(&str, &str)] = &[
    ("Description", "Description"),
    ("Goal", "Goal"),
    ("BurnRateConfigurations", "BurnRateConfigurations"),
    ("AutoInvestigationEnabled", "AutoInvestigationEnabled"),
];

/// The property the objective's NAME is declared in, which is the member the create takes it
/// as — and the one property the update carries no member for, the objective being addressed
/// by its `Id`.
const SLO_NAME_PROPERTY: &str = "Name";

/// The two SLI structures the TYPE publishes, and the CONFIG members the create and the update
/// take them as, each with the metric member that structure renames inside it: a read answers
/// `Sli { SliMetric }` while a write takes `SliConfig { SliMetricConfig }`, and the same for
/// the request-based pair.
const SLI_PROPERTY: &str = "Sli";
const SLI_CONFIG_MEMBER: &str = "SliConfig";
const SLI_METRIC_PROPERTY: &str = "SliMetric";
const SLI_METRIC_CONFIG_MEMBER: &str = "SliMetricConfig";
const REQUEST_BASED_SLI_PROPERTY: &str = "RequestBasedSli";
const REQUEST_BASED_SLI_CONFIG_MEMBER: &str = "RequestBasedSliConfig";
const REQUEST_BASED_SLI_METRIC_PROPERTY: &str = "RequestBasedSliMetric";
const REQUEST_BASED_SLI_METRIC_CONFIG_MEMBER: &str = "RequestBasedSliMetricConfig";

/// The property the exclusion windows of an objective are declared in, the members the one
/// call that writes them takes, and the member of one window that is an INSTANT.
const EXCLUSION_WINDOWS_PROPERTY: &str = "ExclusionWindows";
const SLO_IDS_MEMBER: &str = "SloIds";
const ADD_EXCLUSION_WINDOWS_MEMBER: &str = "AddExclusionWindows";
const REMOVE_EXCLUSION_WINDOWS_MEMBER: &str = "RemoveExclusionWindows";
const EXCLUSION_WINDOW_START_TIME_MEMBER: &str = "StartTime";

/// The attributes an objective publishes that Application Signals reports as SCALARS, as
/// `(attribute, member)`: the ARN it keyed the objective by and the two values it settled from
/// the indicator the declaration states.
const SLO_ATTRIBUTE_MEMBERS: &[(&str, &str)] = &[
    ("Arn", "Arn"),
    ("EvaluationType", "EvaluationType"),
    ("MetricSourceType", "MetricSourceType"),
];

/// The attributes an objective publishes that are INSTANTS, as `(attribute, member)`: a
/// timestamp is a value on the `restJson1` wire rather than text, so each is rendered through
/// the one reading of that direction ([`instant_text`]).
const SLO_INSTANT_ATTRIBUTES: &[(&str, &str)] = &[
    ("CreatedTime", "CreatedTime"),
    ("LastUpdatedTime", "LastUpdatedTime"),
];

/// The properties the objective's own read reports back under the same names, for drift
/// detection, as `(property, member)`: the SCALAR members `GetServiceLevelObjective` answers
/// that a declaration also states.
///
/// The indicator structures and the lists are left out: what the read reports are that
/// service's own shapes, and a property the read does not report is not evidence of anything
/// ([`super::drift`]).
const SLO_REPORTED_PROPERTIES: &[(&str, &str)] = &[("Name", "Name"), ("Description", "Description")];

/// Create the objective an `AWS::ApplicationSignals::ServiceLevelObjective` declares, in
/// Application Signals — so a client that deployed the stack finds it in
/// `ListServiceLevelObjectives`, reads it back by the ARN the stack publishes and gets the
/// budget report of it.
///
/// The declaration IS the request once the two indicator structures are renamed
/// ([`declared_slo_members`]), under the NAME CloudFormation decided, with the declared tags
/// beside the ones CloudFormation gives the resource ([`tags_of`]) as the `TagList` that
/// create's own `Tags` member takes. The EXCLUSION WINDOWS the declaration states are no
/// member of that call, so they are added to the objective the create answered, by the one
/// call that writes them.
pub(super) async fn create_slo(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, SLO_MAPPED_PROPERTIES)?;
    let mut request = declared_slo_members(&resource.properties);
    request.insert(
        SLO_NAME_PROPERTY.to_string(),
        Value::String(slo_name(&resource.properties, resource)),
    );
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            "Tags".to_string(),
            tags_as_list(&tags, UPPER_CASED_TAG),
        );
    }
    let answered = call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        CREATE_SLO,
        "POST",
        SLO_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let attributes = slo_attributes(&answered);
    let Some(arn) = attributes.get("Arn").cloned() else {
        // There would be no handle a client could read the objective back by, and nothing
        // for the stack to publish, so this is a failed create rather than a created
        // resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the CloudWatch Application Signals control plane answered a created \
                      service level objective without an ARN"
                .to_string(),
        });
    };
    if let Some(windows) = declared_exclusion_windows(&resource.properties) {
        // A window the objective could not take leaves an objective the stack did not ask
        // for, so the create is rolled back: the record is removed and the refusal is the
        // resource's.
        if let Err(refusal) =
            write_exclusion_windows(ctx, region, &arn, Some(&windows), None).await
        {
            let removal = delete_slo(ctx, region, &arn).await;
            return Err(rolled_back(refusal, removal, "service level objective"));
        }
    }
    Ok(Created {
        // "Ref returns the ARN" of the objective, which is the handle every Application
        // Signals call on one takes as its `Id`.
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration to the objective the stack created, through
/// `UpdateServiceLevelObjective` — the one call that changes an objective that exists.
///
/// It takes the objective's `Id` together with the description, either indicator config, the
/// goal, the burn-rate configurations and the investigation flag: everything the create takes
/// but the NAME, which is why a re-declared name REPLACES the objective
/// ([`super::template`]'s `name_is_create_only`) rather than reaching here. The exclusion
/// windows are written by their own call, with the windows the previous declaration asked for
/// removed and the ones this declaration asks for added — one call, which is the shape that
/// operation takes.
///
/// `Ok(None)` is a change that call cannot make, so the deployment reports it as not applied
/// rather than as a success that dropped it: a changed `Tags` list, which no member of the
/// update carries — an objective's tags are that service's own tagging calls.
pub(super) async fn update_slo(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if declared.get("Tags") != resource.properties.get("Tags") {
        return Ok(None);
    }
    // The same refusal the create is held to, over the document THIS deployment declares: a
    // property no arm applies must not be reported as an applied change any more than as a
    // created resource.
    refuse_unmapped_properties(
        &StoredResource {
            properties: declared.clone(),
            ..resource.clone()
        },
        SLO_MAPPED_PROPERTIES,
    )?;
    let request = declared_slo_members(declared);
    let answered = call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        UPDATE_SLO,
        "PATCH",
        &slo_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    let held = declared_exclusion_windows(&resource.properties);
    let asked_for = declared_exclusion_windows(declared);
    if held != asked_for {
        write_exclusion_windows(
            ctx,
            region,
            &resource.physical_resource_id,
            asked_for.as_ref(),
            held.as_ref(),
        )
        .await?;
    }
    Ok(Some(Created::attributes(slo_attributes(&answered))))
}

/// Delete the objective a stack created, by the ARN it is addressed by — this resource's
/// physical id. One that service no longer holds (`ResourceNotFoundException`, judged on the
/// CODE and never on a message that carries the request's own values) is already in the state
/// the delete asks for.
pub(super) async fn delete_slo(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        DELETE_SLO,
        "DELETE",
        &slo_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == APPLICATION_SIGNALS_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The objective's configuration as Application Signals reports it, for drift detection:
/// `GetServiceLevelObjective`'s own record, read by the ARN the stack published
/// ([`SLO_REPORTED_PROPERTIES`]). One that service no longer holds was deleted, which is what
/// `None` reports.
pub(super) async fn describe_slo(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        GET_SLO,
        "GET",
        &slo_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(answer) => Ok(answer
            .get(SLO_MEMBER)
            .map(|slo| reported_as_properties(slo, SLO_REPORTED_PROPERTIES))),
        Err(refusal) if refusal.code == APPLICATION_SIGNALS_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI one objective is addressed at: the collection and the `Id` `@httpLabel`, escaped as
/// a single path segment ([`percent_encoded`]) so an ARN's own `:` and `/` address one
/// objective rather than another path.
fn slo_uri(id: &str) -> String {
    format!("{SLO_URI}/{}", percent_encoded(id))
}

/// The NAME the objective is created under: the `Name` the declaration states, or the
/// generated `<stack>-<logical id>-<suffix>` name CloudFormation decided for a declaration
/// that states none.
///
/// This resource's physical id is the objective's ARN, whose resource path is
/// `slo/<name>` — so a record with no declared name falls back to that last path segment,
/// which before the create is the computed name itself.
fn slo_name(declared: &Value, resource: &StoredResource) -> String {
    if let Some(name) = declared
        .get(SLO_NAME_PROPERTY)
        .and_then(scalar_text)
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    resource
        .physical_resource_id
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// One declared property document as the members the objective's create and update both take:
/// the four copied through under their own names, and either indicator structure renamed onto
/// the config member that call takes it as — so one reader serves both calls, and the name,
/// the tags and the exclusion windows (which neither call carries) are left to their own
/// callers.
fn declared_slo_members(declared: &Value) -> Map<String, Value> {
    let mut request = Map::new();
    let Some(properties) = declared.as_object() else {
        // A declaration that is not an object states no member of the request.
        return request;
    };
    copy_members(properties, SLO_COPIED_MEMBERS, &mut request);
    if let Some(sli) = properties.get(SLI_PROPERTY) {
        request.insert(
            SLI_CONFIG_MEMBER.to_string(),
            with_renamed_member(sli, SLI_METRIC_PROPERTY, SLI_METRIC_CONFIG_MEMBER),
        );
    }
    if let Some(sli) = properties.get(REQUEST_BASED_SLI_PROPERTY) {
        request.insert(
            REQUEST_BASED_SLI_CONFIG_MEMBER.to_string(),
            with_renamed_member(
                sli,
                REQUEST_BASED_SLI_METRIC_PROPERTY,
                REQUEST_BASED_SLI_METRIC_CONFIG_MEMBER,
            ),
        );
    }
    request
}

/// One declared structure with the member `property` carried under the name `member`, every
/// other member as written.
///
/// Named apart from [`renamed_member`], the lower-camel re-casing lookup of
/// [`super::application_services`]: this is a shallow rename of ONE member of a structure,
/// which is what the two SLI shapes differ by.
///
/// A declaration that is not an object, or that does not carry that member, passes through
/// unchanged: what reaches the owning operation is what the declaration said, so a document
/// its own validation refuses is refused THERE rather than silently reshaped here.
fn with_renamed_member(declared: &Value, property: &str, member: &str) -> Value {
    let Some(members) = declared.as_object() else {
        return declared.clone();
    };
    Value::Object(
        members
            .iter()
            .map(|(name, value)| {
                let name = if name == property {
                    member.to_string()
                } else {
                    name.clone()
                };
                (name, value.clone())
            })
            .collect(),
    )
}

/// The exclusion windows a declaration states, as the list the one call that writes them
/// takes: each window as declared, with the instant it may start at converted to the seconds
/// that member carries on the wire ([`epoch_seconds`]).
///
/// `None` for a declaration that states none, which is what makes an update that adds or drops
/// the property a change rather than a list compared against an empty one.
fn declared_exclusion_windows(declared: &Value) -> Option<Value> {
    let Some(Value::Array(windows)) = declared.get(EXCLUSION_WINDOWS_PROPERTY) else {
        return None;
    };
    Some(Value::Array(
        windows
            .iter()
            .map(|window| {
                let Some(members) = window.as_object() else {
                    return window.clone();
                };
                Value::Object(
                    members
                        .iter()
                        .map(|(member, value)| {
                            let value = if member == EXCLUSION_WINDOW_START_TIME_MEMBER {
                                value
                                    .as_str()
                                    .and_then(epoch_seconds)
                                    .unwrap_or_else(|| value.clone())
                            } else {
                                value.clone()
                            };
                            (member.clone(), value)
                        })
                        .collect(),
                )
            })
            .collect(),
    ))
}

/// Write the exclusion windows of ONE objective: the windows to add and the ones to remove, in
/// the single call that takes both ("Add or remove time window exclusions for one or more
/// Service Level Objectives").
async fn write_exclusion_windows(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    id: &str,
    add: Option<&Value>,
    remove: Option<&Value>,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        SLO_IDS_MEMBER.to_string(),
        Value::Array(vec![Value::String(id.to_string())]),
    );
    if let Some(add) = add {
        request.insert(ADD_EXCLUSION_WINDOWS_MEMBER.to_string(), add.clone());
    }
    if let Some(remove) = remove {
        request.insert(REMOVE_EXCLUSION_WINDOWS_MEMBER.to_string(), remove.clone());
    }
    call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        BATCH_UPDATE_EXCLUSION_WINDOWS,
        "PATCH",
        EXCLUSION_WINDOWS_URI,
        Some(&Value::Object(request)),
    )
    .await
    .map(|_| ())
}

/// The attributes one answered objective gives the resource: the three scalars Application
/// Signals reports for it and the two instants its clock recorded, each rendered as the text a
/// CloudFormation attribute carries. A member the answer does not carry contributes nothing, so
/// the resource keeps what the naming rules computed rather than taking an empty value.
fn slo_attributes(answered: &Value) -> BTreeMap<String, String> {
    let Some(slo) = answered.get(SLO_MEMBER) else {
        return BTreeMap::new();
    };
    let mut attributes = reported_attributes(slo, SLO_ATTRIBUTE_MEMBERS);
    for (attribute, member) in SLO_INSTANT_ATTRIBUTES {
        if let Some(instant) = slo.get(*member).and_then(instant_text) {
            attributes.insert((*attribute).to_string(), instant);
        }
    }
    attributes
}

// ---------------------------------------------------------------------------
// AWS::ApplicationSignals::GroupingConfiguration
// ---------------------------------------------------------------------------

/// The collection the account's grouping configuration is bound to (`PUT`/`DELETE
/// /grouping-configuration`), and the one the definitions it holds are READ from
/// (`POST /grouping-attribute-definitions`).
const GROUPING_CONFIGURATION_URI: &str = "/grouping-configuration";
const GROUPING_ATTRIBUTE_DEFINITIONS_URI: &str = "/grouping-attribute-definitions";

/// The operations this module calls on that configuration, as Application Signals' own model
/// names them.
const PUT_GROUPING_CONFIGURATION: &str = "PutGroupingConfiguration";
const DELETE_GROUPING_CONFIGURATION: &str = "DeleteGroupingConfiguration";
const LIST_GROUPING_ATTRIBUTE_DEFINITIONS: &str = "ListGroupingAttributeDefinitions";

/// The one property the type publishes, which is the one member the put requires, and the
/// member the put answers the stored configuration in.
const GROUPING_ATTRIBUTE_DEFINITIONS: &str = "GroupingAttributeDefinitions";
const GROUPING_CONFIGURATION_MEMBER: &str = "GroupingConfiguration";

/// The one attribute the type publishes, which is the member the configuration carries it in.
const UPDATED_AT_ATTRIBUTE: &str = "UpdatedAt";

/// Put the grouping configuration an `AWS::ApplicationSignals::GroupingConfiguration`
/// declares, so the account's own `ListGroupingAttributeDefinitions` reports the definitions
/// the template asked for and the services Application Signals discovers are grouped by them.
///
/// The one call is documented "Creates or updates the grouping configuration for this account",
/// so it is this type's UPDATE as well as its create: the account holds ONE configuration, and
/// a stack that re-declares the definitions overwrites the ones in force rather than making a
/// second configuration. The resource keeps the identity CloudFormation computed for it — the
/// account whose configuration it is — so this arm answers attributes alone.
pub(super) async fn put_grouping_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(
        &StoredResource {
            properties: declared.clone(),
            ..resource.clone()
        },
        &[GROUPING_ATTRIBUTE_DEFINITIONS],
    )?;
    let mut request = Map::new();
    if let Some(definitions) = declared.get(GROUPING_ATTRIBUTE_DEFINITIONS) {
        request.insert(
            GROUPING_ATTRIBUTE_DEFINITIONS.to_string(),
            definitions.clone(),
        );
    }
    let answered = call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        PUT_GROUPING_CONFIGURATION,
        "PUT",
        GROUPING_CONFIGURATION_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Created::attributes(grouping_configuration_attributes(
        answered.get(GROUPING_CONFIGURATION_MEMBER),
    )))
}

/// Delete the account's grouping configuration, which "removes all custom grouping attribute
/// definitions" — the one call that takes the configuration away. It takes no identifier at
/// all: there is one configuration per account and Region, which is the resource the stack
/// declared.
pub(super) async fn delete_grouping_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        DELETE_GROUPING_CONFIGURATION,
        "DELETE",
        GROUPING_CONFIGURATION_URI,
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == APPLICATION_SIGNALS_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The grouping configuration as Application Signals reports it, for drift detection: the
/// definitions its own read answers for this account. A configuration whose definitions it no
/// longer reports was deleted — the delete is documented as removing all of them — which is
/// what `None` reports.
pub(super) async fn describe_grouping_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Result<Option<Value>, Refusal> {
    let answered = call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        LIST_GROUPING_ATTRIBUTE_DEFINITIONS,
        "POST",
        GROUPING_ATTRIBUTE_DEFINITIONS_URI,
        Some(&Value::Object(Map::new())),
    )
    .await?;
    let Some(Value::Array(definitions)) = answered.get(GROUPING_ATTRIBUTE_DEFINITIONS) else {
        return Ok(None);
    };
    if definitions.is_empty() {
        return Ok(None);
    }
    Ok(Some(reported_as_properties(
        &answered,
        &[(
            GROUPING_ATTRIBUTE_DEFINITIONS,
            GROUPING_ATTRIBUTE_DEFINITIONS,
        )],
    )))
}

/// The attributes one answered grouping configuration gives the resource: the instant that
/// service recorded for it, rendered as the text a CloudFormation attribute carries. A
/// configuration it reported nothing about gives none, so the resource keeps what the naming
/// rules computed.
fn grouping_configuration_attributes(configuration: Option<&Value>) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    if let Some(updated_at) = configuration
        .and_then(|configuration| configuration.get(UPDATED_AT_ATTRIBUTE))
        .and_then(instant_text)
    {
        attributes.insert(UPDATED_AT_ATTRIBUTE.to_string(), updated_at);
    }
    attributes
}

// ---------------------------------------------------------------------------
// AWS::ApplicationSignals::InstrumentationConfig
// ---------------------------------------------------------------------------

/// The URIs the dynamic instrumentation calls are bound to, each an operation of its own
/// (`POST`, as that model's `@http` traits declare).
const CREATE_INSTRUMENTATION_CONFIGURATION_URI: &str = "/create-instrumentation-configuration";
const GET_INSTRUMENTATION_CONFIGURATION_URI: &str = "/get-instrumentation-configuration";
const DELETE_INSTRUMENTATION_CONFIGURATION_URI: &str = "/delete-instrumentation-configuration";

/// The operations this module calls on an instrumentation configuration, as Application
/// Signals' own model names them.
const CREATE_INSTRUMENTATION_CONFIGURATION: &str = "CreateInstrumentationConfiguration";
const GET_INSTRUMENTATION_CONFIGURATION: &str = "GetInstrumentationConfiguration";
const DELETE_INSTRUMENTATION_CONFIGURATION: &str = "DeleteInstrumentationConfiguration";

/// The member the read answers the stored record in.
const CONFIGURATION_MEMBER: &str = "Configuration";

/// The properties this arm maps onto the create's members, and so the ones a declaration may
/// carry. They ARE that call's own members, spelled identically on both sides.
///
/// Source: the type's own entry in the "AWS resource and property types reference" read beside
/// `CreateInstrumentationConfiguration`'s own members (`smithy_query(query="operation",
/// operation="CreateInstrumentationConfiguration")`).
const INSTRUMENTATION_CONFIG_MAPPED_PROPERTIES: &[&str] = &[
    "AttributeFilters",
    "CaptureConfiguration",
    "Description",
    "Environment",
    "ExpiresAt",
    "InstrumentationType",
    "Location",
    "Service",
    "SignalType",
    "Tags",
];

/// The members the read and the delete address ONE configuration by, beside the location: the
/// four identity dimensions the create's documented conflict is on.
const INSTRUMENTATION_IDENTITY_MEMBERS: &[(&str, &str)] = &[
    ("InstrumentationType", "InstrumentationType"),
    ("Service", "Service"),
    ("Environment", "Environment"),
    ("SignalType", "SignalType"),
];

/// The member the read and the delete take the location under, and the member of that union
/// this arm names the point by: the `LocationHash` the create answered, which is the value
/// that service computed from the location itself.
const LOCATION_IDENTIFIER_MEMBER: &str = "LocationIdentifier";
const LOCATION_HASH_MEMBER: &str = "LocationHash";

/// The one property of this type that states an INSTANT, which the template spells as
/// ISO-8601 text while the member carries the seconds since the epoch.
const EXPIRES_AT_PROPERTY: &str = "ExpiresAt";

/// The attributes an instrumentation configuration publishes that Application Signals reports
/// as SCALARS, as `(attribute, member)`: the ARN it minted — whose own member is spelled `ARN`
/// — and the location hash it computed.
const INSTRUMENTATION_ATTRIBUTE_MEMBERS: &[(&str, &str)] =
    &[("Arn", "ARN"), ("LocationHash", "LocationHash")];

/// The one attribute of this type that is an INSTANT: the instant that service recorded for
/// the record it created.
const CREATED_AT_ATTRIBUTE: &str = "CreatedAt";

/// The properties the configuration's own read reports back under the same names, for drift
/// detection, as `(property, member)`: the SCALAR members
/// `GetInstrumentationConfiguration` answers that a declaration also states.
const INSTRUMENTATION_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("InstrumentationType", "InstrumentationType"),
    ("Service", "Service"),
    ("Environment", "Environment"),
    ("SignalType", "SignalType"),
    ("Description", "Description"),
];

/// Create the instrumentation point an `AWS::ApplicationSignals::InstrumentationConfig`
/// declares, in Application Signals — so the SDK of the instrumented service finds it in
/// `ListInstrumentationConfigurations` for the service and environment the declaration names,
/// and the capture the template asked for really happens at that code location.
///
/// The declaration IS the request ([`INSTRUMENTATION_CONFIG_MAPPED_PROPERTIES`]), with the one
/// instant it may state converted to the seconds that member carries and the declared tags
/// beside the ones CloudFormation gives the resource ([`tags_of`]) as the `TagList` that
/// create's own `Tags` member takes.
///
/// The identity is that service's: the ARN it minted — which carries the `LocationHash` it
/// computed — becomes this resource's physical id, and the three values the type publishes are
/// read off the record it answered.
pub(super) async fn create_instrumentation_config(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, INSTRUMENTATION_CONFIG_MAPPED_PROPERTIES)?;
    let mut request = match resource.properties.as_object() {
        Some(properties) => properties.clone(),
        None => Map::new(),
    };
    request.remove("Tags");
    if let Some(expires_at) = request
        .get(EXPIRES_AT_PROPERTY)
        .and_then(|value| value.as_str().and_then(epoch_seconds))
    {
        request.insert(EXPIRES_AT_PROPERTY.to_string(), expires_at);
    }
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert("Tags".to_string(), tags_as_list(&tags, UPPER_CASED_TAG));
    }
    let answered = call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        CREATE_INSTRUMENTATION_CONFIGURATION,
        "POST",
        CREATE_INSTRUMENTATION_CONFIGURATION_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let attributes = instrumentation_attributes(&answered);
    let Some(arn) = attributes.get("Arn").cloned() else {
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the CloudWatch Application Signals control plane answered a created \
                      instrumentation configuration without an ARN"
                .to_string(),
        });
    };
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Delete the instrumentation point a stack created, by the identity the stack holds for it:
/// the four dimensions its declaration states and the location hash the create answered. One
/// that service no longer holds (`ResourceNotFoundException`, judged on the CODE) is already in
/// the state the delete asks for.
pub(super) async fn delete_instrumentation_config(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        DELETE_INSTRUMENTATION_CONFIGURATION,
        "POST",
        DELETE_INSTRUMENTATION_CONFIGURATION_URI,
        Some(&instrumentation_identity(resource)),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == APPLICATION_SIGNALS_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The configuration as Application Signals reports it, for drift detection:
/// `GetInstrumentationConfiguration`'s own record, read by the identity the stack holds
/// ([`INSTRUMENTATION_REPORTED_PROPERTIES`]). One that service no longer holds was deleted.
pub(super) async fn describe_instrumentation_config(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        APPLICATION_SIGNALS_SIGNING_NAME,
        region,
        GET_INSTRUMENTATION_CONFIGURATION,
        "POST",
        GET_INSTRUMENTATION_CONFIGURATION_URI,
        Some(&instrumentation_identity(resource)),
    )
    .await
    {
        Ok(answer) => Ok(answer.get(CONFIGURATION_MEMBER).map(|configuration| {
            reported_as_properties(configuration, INSTRUMENTATION_REPORTED_PROPERTIES)
        })),
        Err(refusal) if refusal.code == APPLICATION_SIGNALS_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The request both the read and the delete of ONE configuration take: the four identity
/// dimensions the declaration states, and the location named by the hash that service computed
/// — "the pre-computed location hash", which is one of the two forms its `LocationIdentifier`
/// admits.
///
/// The hash is the `LocationHash` attribute the create reported; a record that carries none
/// falls back to the last segment of this resource's physical id, which is the ARN's own final
/// `[0-9a-f]{16}` segment.
fn instrumentation_identity(resource: &StoredResource) -> Value {
    let mut request = Map::new();
    if let Some(properties) = resource.properties.as_object() {
        copy_members(properties, INSTRUMENTATION_IDENTITY_MEMBERS, &mut request);
    }
    let mut identifier = Map::new();
    identifier.insert(
        LOCATION_HASH_MEMBER.to_string(),
        Value::String(instrumentation_location_hash(resource)),
    );
    request.insert(
        LOCATION_IDENTIFIER_MEMBER.to_string(),
        Value::Object(identifier),
    );
    Value::Object(request)
}

/// The `LocationHash` the stack holds for one configuration: see [`instrumentation_identity`].
fn instrumentation_location_hash(resource: &StoredResource) -> String {
    if let Some(hash) = resource.attributes.get(LOCATION_HASH_MEMBER) {
        return hash.clone();
    }
    resource
        .physical_resource_id
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The attributes one answered instrumentation configuration gives the resource: the ARN and
/// the location hash that service reported, plus the instant its clock recorded, rendered as
/// the text a CloudFormation attribute carries.
fn instrumentation_attributes(answered: &Value) -> BTreeMap<String, String> {
    let mut attributes = reported_attributes(answered, INSTRUMENTATION_ATTRIBUTE_MEMBERS);
    if let Some(created_at) = answered.get(CREATED_AT_ATTRIBUTE).and_then(instant_text) {
        attributes.insert(CREATED_AT_ATTRIBUTE.to_string(), created_at);
    }
    attributes
}
