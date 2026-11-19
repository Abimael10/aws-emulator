// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Billing and Cost Management Dashboards control
//! plane: the dashboard an `AWS::BCM::Dashboard` declares and the scheduled report an
//! `AWS::BCM::ScheduledReport` declares. One file of [`super`]'s per-type arms, dispatched
//! to by its `Provisioner`.
//!
//! ## Why both resources are really created
//!
//! `CreateDashboard` takes the dashboard's name, description and widgets and answers the
//! `arn` that service MINTED for it; `CreateScheduledReport` takes the report's whole
//! configuration — the dashboard it reports on among it — and answers the `arn` it minted
//! for the report. Each ARN is the handle every later call on the resource takes
//! (`GetDashboard`, `UpdateDashboard`, `DeleteDashboard`, and the matching three for a
//! report, all addressed by `arn`), and the identity that type's `Ref` returns, so it
//! becomes the resource's physical id. A stack that reported either as created while
//! nothing created it publishes an ARN that resolves to nothing: no dashboard to read, no
//! report to execute, and no tag set on either.
//!
//! ## The properties
//!
//! The type's properties are the owning API's own members with each initial upper-cased,
//! which is how a resource type spells them: the dashboard's `Name`, `Description` and
//! `Widgets`, and the report's `Name`, `DashboardArn`,
//! `ScheduledReportExecutionRoleArn`, `ScheduleConfig`, `Description`, `WidgetIds` and
//! `WidgetDateRangeOverride`. A report's configuration travels inside the
//! `scheduledReport` member its create and no other wraps it in; a dashboard's members are
//! that create's own.
//!
//! The declared tag set is the `Tags` CloudFormation's tagging convention gives every
//! taggable type ([`tags_of`]), carried onto the `resourceTags` member both creates take —
//! and reconciled afterwards through that service's own `TagResource`/`UntagResource`,
//! which is what a stack's own tags change through ([`retag_bcm_resource`]).
//!
//! A declared property neither call carries is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`]): a dashboard reported created with fewer widgets than
//! its declaration asked for is the shallow success this arm exists to avoid.
//!
//! ## What a declaration that service refuses does
//!
//! Nothing here checks a declared value: every constraint is that service's own — a
//! widget's `title` is `^(?!.* {2})[a-zA-Z0-9_-][ a-zA-Z0-9_-]*[a-zA-Z0-9_-]$`, a
//! dashboard's `name` must be unique in the account, a report's `scheduleExpression` must
//! be a six-field cron expression and its `dashboardArn` must name a dashboard that
//! exists — so a declaration outside one of them is refused BY that service, and that
//! refusal is what fails the resource and rolls the stack back. One check in one place,
//! worded as that service words it.

use super::*;

/// The signing name of that service, from its model (`sigv4Name: bcm-dashboards`), which
/// is also the ARN namespace its resources carry, and the `X-Amz-Target` prefix its
/// service shape's own name gives (`AWSBCMDashboardsService`): it speaks `awsJson1_0`, so
/// its calls ride on [`call_json_rpc`].
const BCM_DASHBOARDS_SIGNING_NAME: &str = "bcm-dashboards";
const BCM_DASHBOARDS_TARGET_PREFIX: &str = "AWSBCMDashboardsService";

/// The operations this module calls, as that service's own model names them.
const CREATE_DASHBOARD: &str = "CreateDashboard";
const GET_DASHBOARD: &str = "GetDashboard";
const UPDATE_DASHBOARD: &str = "UpdateDashboard";
const DELETE_DASHBOARD: &str = "DeleteDashboard";
const CREATE_SCHEDULED_REPORT: &str = "CreateScheduledReport";
const GET_SCHEDULED_REPORT: &str = "GetScheduledReport";
const UPDATE_SCHEDULED_REPORT: &str = "UpdateScheduledReport";
const DELETE_SCHEDULED_REPORT: &str = "DeleteScheduledReport";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code that service answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its reads, of
/// `UpdateScheduledReport` and of `DeleteScheduledReport`), which is what says a delete
/// has nothing left to remove and a read has no resource to report.
const BCM_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member every call on one resource of that service addresses it by, and the member
/// each create answers the ARN it minted in.
const ARN_MEMBER: &str = "arn";

/// The member `GetScheduledReport` wraps its record in, and the member
/// `CreateScheduledReport` takes the report's configuration in.
const SCHEDULED_REPORT_MEMBER: &str = "scheduledReport";

/// The member both creates take their tag set in, which is the list of `ResourceTag`
/// structures `TagResource` takes too — whose members are spelled `key`/`value`
/// ([`LOWER_CASED_TAG`]).
const RESOURCE_TAGS_MEMBER: &str = "resourceTags";

/// The member `UntagResource` takes the keys to drop in, and the member both tagging calls
/// address a resource by.
const RESOURCE_TAG_KEYS_MEMBER: &str = "resourceTagKeys";
const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// The SCALAR properties of `AWS::BCM::Dashboard` that are members of `CreateDashboard`
/// and of `UpdateDashboard` alike, each under the member of the same name with its initial
/// lower-cased.
///
/// `Widgets` is not among them: it is a list of structures, re-cased member by member
/// ([`declared_widgets`]).
const DASHBOARD_MEMBERS: &[(&str, &str)] = &[("Description", "description"), ("Name", "name")];

/// The property a dashboard's widgets are declared in, and the member both calls take them
/// as.
const WIDGETS_PROPERTY: &str = "Widgets";
const WIDGETS_MEMBER: &str = "widgets";

/// Every property of `AWS::BCM::Dashboard` this arm carries onto the owning API, so a
/// declaration stating one outside the set is REFUSED rather than reported as created
/// without it ([`refuse_unmapped_properties`]).
const DASHBOARD_MAPPED_PROPERTIES: &[&str] = &["Description", "Name", "Tags", "Widgets"];

/// The SCALAR and LIST-OF-SCALAR properties of `AWS::BCM::ScheduledReport` that are
/// members of its create's `scheduledReport` and of `UpdateScheduledReport` alike, each
/// under the member of the same name with its initial lower-cased.
///
/// The two STRUCTURE properties are not among them: the schedule is re-cased with its two
/// instants put onto the wire ([`schedule_config_member`]) and the date-range override is
/// re-cased member by member.
const SCHEDULED_REPORT_MEMBERS: &[(&str, &str)] = &[
    ("DashboardArn", "dashboardArn"),
    ("Description", "description"),
    ("Name", "name"),
    (
        "ScheduledReportExecutionRoleArn",
        "scheduledReportExecutionRoleArn",
    ),
    ("WidgetIds", "widgetIds"),
];

/// The two structure properties of that type, the members they travel as, and the flags
/// `UpdateScheduledReport` clears each one by: "set to true to clear existing widget IDs"
/// and the same for the date-range override.
const SCHEDULE_CONFIG_PROPERTY: &str = "ScheduleConfig";
const SCHEDULE_CONFIG_MEMBER: &str = "scheduleConfig";
const WIDGET_DATE_RANGE_PROPERTY: &str = "WidgetDateRangeOverride";
const WIDGET_DATE_RANGE_MEMBER: &str = "widgetDateRangeOverride";
const WIDGET_IDS_PROPERTY: &str = "WidgetIds";
const CLEAR_WIDGET_IDS_MEMBER: &str = "clearWidgetIds";
const CLEAR_WIDGET_DATE_RANGE_MEMBER: &str = "clearWidgetDateRangeOverride";

/// Every property of `AWS::BCM::ScheduledReport` this arm carries onto the owning API, for
/// the reason [`DASHBOARD_MAPPED_PROPERTIES`] records.
const SCHEDULED_REPORT_MAPPED_PROPERTIES: &[&str] = &[
    "DashboardArn",
    "Description",
    "Name",
    "ScheduleConfig",
    "ScheduledReportExecutionRoleArn",
    "Tags",
    "WidgetDateRangeOverride",
    "WidgetIds",
];

/// The member of `ScheduleConfig` whose own two members are INSTANTS, and those two
/// members: `SchedulePeriod$startTime` and `$endTime` are `GenericTimeStamp`s, which an
/// awsJson wire carries as the seconds since the epoch while a template states an instant
/// as ISO-8601 text ([`epoch_seconds`]).
const SCHEDULE_PERIOD_MEMBER: &str = "schedulePeriod";
const SCHEDULE_PERIOD_INSTANTS: &[&str] = &["startTime", "endTime"];

/// The members of one widget that are NUMBERS on the wire (`WidgetWidth`, `WidgetHeight`
/// and the `Integer` offset), read in the one spelling those members take
/// ([`declared_number`]) so a width a parameter reference resolved to text still reaches
/// the integral member it is.
const WIDGET_NUMBER_MEMBERS: &[&str] = &["width", "height", "horizontalOffset"];

/// The member of a widget's `DisplayConfig` whose MAP keys are data rather than modelled
/// member names: `GraphDisplayConfigMap` is keyed by the metric the graph configuration
/// applies to, so those keys are carried verbatim while every member name around them is
/// re-cased ([`lower_camel_members_except_data_keys`]).
const DATA_KEYED_MEMBERS: &[&str] = &["graph"];

/// The attributes an `AWS::BCM::Dashboard` publishes beside the ARN it is identified by:
/// the kind of dashboard that service records it as and the two instants of its clock,
/// each read off its own report of the dashboard rather than computed here.
const DASHBOARD_REPORTED_ATTRIBUTES: &[(&str, &str)] = &[("Type", "type")];
const DASHBOARD_INSTANT_ATTRIBUTES: &[(&str, &str)] =
    &[("CreatedAt", "createdAt"), ("UpdatedAt", "updatedAt")];

/// The attributes an `AWS::BCM::ScheduledReport` publishes beside its ARN: the two
/// instants of that service's clock.
const SCHEDULED_REPORT_INSTANT_ATTRIBUTES: &[(&str, &str)] =
    &[("CreatedAt", "createdAt"), ("UpdatedAt", "updatedAt")];

/// The ARN attribute both types publish, which is also the identity each type's `Ref`
/// returns.
const ARN_ATTRIBUTE: &str = "Arn";

/// The scalar properties `GetDashboard` reports back under the names the type declares,
/// for drift detection. `Widgets` is a LIST on the wire and is left out, which is the
/// position every arm whose property is not a scalar is in and what [`drift`] documents.
const DASHBOARD_REPORTED_PROPERTIES: &[(&str, &str)] =
    &[("Description", "description"), ("Name", "name")];

/// The scalar properties `GetScheduledReport` reports back under the names the type
/// declares, for the same reading. The schedule, the override and the widget ids are a
/// structure or a list on the wire and are left out for the reason above.
const SCHEDULED_REPORT_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("DashboardArn", "dashboardArn"),
    ("Description", "description"),
    ("Name", "name"),
    (
        "ScheduledReportExecutionRoleArn",
        "scheduledReportExecutionRoleArn",
    ),
];

/// Create the dashboard an `AWS::BCM::Dashboard` declares, in that service itself — so a
/// client that deployed the stack reads the dashboard back through its own `GetDashboard`
/// and `ListDashboards` for the ARN the stack published, schedules reports on it and tags
/// it.
///
/// The declaration IS the request: the two scalar members ([`DASHBOARD_MEMBERS`]) and the
/// widgets re-cased onto the `widgets` member, with the declared tags beside the ones
/// CloudFormation gives the resource ([`tags_of`]) as the `resourceTags` list that call
/// takes.
///
/// The identity is that service's: the `arn` it minted becomes this resource's physical id
/// — the type's primary identifier, which its `Ref` returns — and the values the type
/// publishes are read off its own report of the dashboard it has just stored.
pub(super) async fn create_bcm_dashboard(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, DASHBOARD_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = dashboard_request(&properties)?;
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            RESOURCE_TAGS_MEMBER.to_string(),
            tags_as_list(&tags, LOWER_CASED_TAG),
        );
    }
    let answered = call_bcm(ctx, region, CREATE_DASHBOARD, &Value::Object(request)).await?;
    let arn = minted_arn(&answered, "dashboard")?;
    let reported = read_bcm_dashboard(ctx, region, &arn).await?;
    let mut attributes = dashboard_attributes(reported.as_ref());
    attributes.insert(ARN_ATTRIBUTE.to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration to the dashboard the stack created, through
/// `UpdateDashboard` — the one call that changes a dashboard that exists.
///
/// It takes the same name, description and widgets the create does, addressed by the `arn`
/// that service minted, so a changed declaration of any of them is applied to that
/// dashboard rather than replacing it; the tags travel separately
/// ([`retag_bcm_resource`]).
pub(super) async fn update_bcm_dashboard(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = refuse_unmapped(resource, declared, DASHBOARD_MAPPED_PROPERTIES)?;
    let mut request = dashboard_request(&properties)?;
    request.insert(
        ARN_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    call_bcm(ctx, region, UPDATE_DASHBOARD, &Value::Object(request)).await?;
    // The update answers only the ARN, so what the resource now reports is read off that
    // service's own report of the dashboard — the one reading the create uses.
    let reported = read_bcm_dashboard(ctx, region, &resource.physical_resource_id).await?;
    let mut attributes = dashboard_attributes(reported.as_ref());
    attributes.insert(
        ARN_ATTRIBUTE.to_string(),
        resource.physical_resource_id.clone(),
    );
    Ok(Created::attributes(attributes))
}

/// Delete the dashboard a stack created, by the ARN it was created under — this resource's
/// physical id. One that service no longer holds is already in the state the delete asks
/// for, judged on the not-found CODE and never on a message that carries the request's own
/// ARN.
pub(super) async fn delete_bcm_dashboard(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    delete_bcm_resource(ctx, region, DELETE_DASHBOARD, physical_resource_id).await
}

/// The dashboard's configuration as that service reports it, for drift detection:
/// `GetDashboard`'s own record, read by the ARN the stack published, projected onto the
/// property names the type declares. One it no longer holds was deleted, which is what
/// `None` reports.
pub(super) async fn describe_bcm_dashboard(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_bcm_dashboard(ctx, region, physical_resource_id)
        .await?
        .map(|dashboard| reported_as_properties(&dashboard, DASHBOARD_REPORTED_PROPERTIES)))
}

/// Create the scheduled report an `AWS::BCM::ScheduledReport` declares, in that service
/// itself — so a client that deployed the stack reads the report back through its own
/// `GetScheduledReport` and `ListScheduledReports` for the ARN the stack published, and
/// `ExecuteScheduledReport` really runs it.
///
/// The declaration IS the `scheduledReport` that create wraps a report's configuration in,
/// with the declared tags beside the ones CloudFormation gives the resource as the
/// `resourceTags` list beside it.
///
/// No `clientToken` is sent. That member is the CALLER's idempotency key and it is
/// optional: a token this provisioner derived from the resource would be presented again
/// by the create half of a replacement, which that service answers as the terminal
/// "already exists" conflict while the report the stack is replacing is still there — so
/// the deployment would fail over a key nobody asked for.
pub(super) async fn create_bcm_scheduled_report(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, SCHEDULED_REPORT_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    request.insert(
        SCHEDULED_REPORT_MEMBER.to_string(),
        Value::Object(scheduled_report_members(&properties)?),
    );
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            RESOURCE_TAGS_MEMBER.to_string(),
            tags_as_list(&tags, LOWER_CASED_TAG),
        );
    }
    let answered = call_bcm(
        ctx,
        region,
        CREATE_SCHEDULED_REPORT,
        &Value::Object(request),
    )
    .await?;
    let arn = minted_arn(&answered, "scheduled report")?;
    let reported = read_bcm_scheduled_report(ctx, region, &arn).await?;
    let mut attributes = scheduled_report_attributes(reported.as_ref());
    attributes.insert(ARN_ATTRIBUTE.to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration to the scheduled report the stack created, through
/// `UpdateScheduledReport` — the one call that changes a report that exists.
///
/// Its members are the report's own, addressed by the `arn` that service minted, so a
/// changed name, description, dashboard, execution role, schedule, widget selection or
/// date-range override is applied to that report rather than replacing it. A declaration
/// that no longer states the widget selection or the override asks for the report NOT to
/// carry one, which that call expresses as its two clearing flags rather than as an absent
/// member — and those flags "cannot be true in a request that also specifies" the member
/// they clear, so each is set only where the declaration states none.
pub(super) async fn update_bcm_scheduled_report(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = refuse_unmapped(resource, declared, SCHEDULED_REPORT_MAPPED_PROPERTIES)?;
    let mut request = scheduled_report_members(&properties)?;
    request.insert(
        ARN_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    request.insert(
        CLEAR_WIDGET_IDS_MEMBER.to_string(),
        Value::Bool(!properties.contains_key(WIDGET_IDS_PROPERTY)),
    );
    request.insert(
        CLEAR_WIDGET_DATE_RANGE_MEMBER.to_string(),
        Value::Bool(!properties.contains_key(WIDGET_DATE_RANGE_PROPERTY)),
    );
    call_bcm(
        ctx,
        region,
        UPDATE_SCHEDULED_REPORT,
        &Value::Object(request),
    )
    .await?;
    let reported = read_bcm_scheduled_report(ctx, region, &resource.physical_resource_id).await?;
    let mut attributes = scheduled_report_attributes(reported.as_ref());
    attributes.insert(
        ARN_ATTRIBUTE.to_string(),
        resource.physical_resource_id.clone(),
    );
    Ok(Created::attributes(attributes))
}

/// Delete the scheduled report a stack created, by the ARN it was created under — this
/// resource's physical id. One that service no longer holds
/// (`ResourceNotFoundException`) is already in the state the delete asks for.
pub(super) async fn delete_bcm_scheduled_report(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    delete_bcm_resource(ctx, region, DELETE_SCHEDULED_REPORT, physical_resource_id).await
}

/// The scheduled report's configuration as that service reports it, for drift detection:
/// `GetScheduledReport`'s own record, read by the ARN the stack published.
pub(super) async fn describe_bcm_scheduled_report(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(
        read_bcm_scheduled_report(ctx, region, physical_resource_id)
            .await?
            .map(|report| reported_as_properties(&report, SCHEDULED_REPORT_REPORTED_PROPERTIES)),
    )
}

/// Bring the tags of the dashboard or the scheduled report a resource record names to the
/// ones this deployment gives it, through that service's own `TagResource` and
/// `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the create applied — so a key outside it is one this
/// deployment no longer gives the resource and is removed, and one in it with another
/// value is re-valued. Where the two already agree nothing is called at all, which is what
/// makes this safe to ask on every deployment; a resource that service no longer holds has
/// no tags to reconcile.
///
/// ONE arm for both types: `ResourceArn` names either a `dashboard` or a
/// `scheduled-report`, and those two calls are the only way a resource of that service
/// takes a tag after it exists — neither update carries one.
pub(super) async fn retag_bcm_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let arn = Value::String(resource.physical_resource_id.clone());
    let addressed = Value::Object(
        [(RESOURCE_ARN_MEMBER.to_string(), arn.clone())]
            .into_iter()
            .collect(),
    );
    let answered = match call_bcm(ctx, region, LIST_TAGS_FOR_RESOURCE, &addressed).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == BCM_RESOURCE_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    let held = held_tags(&answered);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        let request = Value::Object(
            [
                (RESOURCE_ARN_MEMBER.to_string(), arn.clone()),
                (
                    RESOURCE_TAGS_MEMBER.to_string(),
                    tags_as_list(&added, LOWER_CASED_TAG),
                ),
            ]
            .into_iter()
            .collect(),
        );
        call_bcm(ctx, region, TAG_RESOURCE, &request).await?;
    }
    if !removed.is_empty() {
        let request = Value::Object(
            [
                (RESOURCE_ARN_MEMBER.to_string(), arn),
                (
                    RESOURCE_TAG_KEYS_MEMBER.to_string(),
                    tag_keys_as_list(&removed),
                ),
            ]
            .into_iter()
            .collect(),
        );
        call_bcm(ctx, region, UNTAG_RESOURCE, &request).await?;
    }
    Ok(())
}

/// The members `CreateDashboard` and `UpdateDashboard` both take, read off one declared
/// property document: the two scalars and the widgets, re-cased onto the API's own member
/// names.
fn dashboard_request(properties: &Map<String, Value>) -> Result<Map<String, Value>, Refusal> {
    let mut request = Map::new();
    copy_members(properties, DASHBOARD_MEMBERS, &mut request);
    if let Some(declared) = properties.get(WIDGETS_PROPERTY) {
        request.insert(WIDGETS_MEMBER.to_string(), declared_widgets(declared)?);
    }
    Ok(request)
}

/// The members a report's configuration travels as, read off one declared property
/// document: the scalars and the widget-id list, the schedule with its instants put onto
/// the wire, and the date-range override re-cased member by member.
fn scheduled_report_members(
    properties: &Map<String, Value>,
) -> Result<Map<String, Value>, Refusal> {
    let mut members = Map::new();
    copy_members(properties, SCHEDULED_REPORT_MEMBERS, &mut members);
    if let Some(declared) = properties.get(SCHEDULE_CONFIG_PROPERTY) {
        members.insert(
            SCHEDULE_CONFIG_MEMBER.to_string(),
            schedule_config_member(declared)?,
        );
    }
    if let Some(declared) = properties.get(WIDGET_DATE_RANGE_PROPERTY) {
        members.insert(
            WIDGET_DATE_RANGE_MEMBER.to_string(),
            api_members(declared)?,
        );
    }
    Ok(members)
}

/// One declared `ScheduleConfig` as the member both calls take it as: its members re-cased
/// ([`api_members`]), with the schedule PERIOD's two instants put onto the wire in the
/// form those members are carried in.
///
/// A template states an instant as ISO-8601 text while `SchedulePeriod`'s members are
/// timestamps, which an awsJson wire carries as the seconds since the epoch — so a
/// declared instant is converted ([`epoch_seconds`]). A value that is not such text is
/// passed through as it came, so a template that already states the seconds reaches that
/// member unchanged and anything else is refused by that operation's own validation rather
/// than silently dropped here.
fn schedule_config_member(declared: &Value) -> Result<Value, Refusal> {
    let mut config = api_members(declared)?;
    let Some(period) = config
        .get_mut(SCHEDULE_PERIOD_MEMBER)
        .and_then(Value::as_object_mut)
    else {
        return Ok(config);
    };
    for member in SCHEDULE_PERIOD_INSTANTS {
        let converted = period
            .get(*member)
            .and_then(Value::as_str)
            .and_then(epoch_seconds);
        if let Some(converted) = converted {
            period.insert((*member).to_string(), converted);
        }
    }
    Ok(config)
}

/// One declared `Widgets` list as the member both dashboard calls take it as: every
/// structure re-cased ([`api_members`]), with each widget's three numeric members read in
/// the spelling they are carried in ([`WIDGET_NUMBER_MEMBERS`]).
fn declared_widgets(declared: &Value) -> Result<Value, Refusal> {
    let mut widgets = api_members(declared)?;
    let Some(items) = widgets.as_array_mut() else {
        // A declaration whose widgets are not a list states no widget; that call's own
        // validation is what refuses it rather than this dropping it.
        return Ok(widgets);
    };
    for widget in items {
        let Some(members) = widget.as_object_mut() else {
            continue;
        };
        for member in WIDGET_NUMBER_MEMBERS {
            if let Some(converted) = members.get(*member).and_then(declared_number) {
                members.insert((*member).to_string(), converted);
            }
        }
    }
    Ok(widgets)
}

/// One declared document as the members the owning API takes: the crate's own re-casing
/// walk ([`lower_camel_members_except_data_keys`]), told which member of this type carries a
/// MAP whose KEYS are the caller's own data rather than modelled member names
/// ([`DATA_KEYED_MEMBERS`]).
///
/// A document nested deeper than that walk's own bound is refused rather than walked, for
/// the reason the bound records — the nesting is the REQUESTER's choice.
fn api_members(value: &Value) -> Result<Value, Refusal> {
    lower_camel_members_except_data_keys(value, 0, DATA_KEYED_MEMBERS)
}

/// The ARN a create answered, or the refusal for an answer that carried none: there would
/// be no handle a client could call that service with, and nothing for the stack to
/// publish, so that is a failed create rather than a created resource.
fn minted_arn(answered: &Value, resource: &str) -> Result<String, Refusal> {
    reported_text(answered, ARN_MEMBER)
        .filter(|arn| !arn.is_empty())
        .ok_or_else(|| Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "the Billing and Cost Management Dashboards control plane answered a created \
                 {resource} without an ARN"
            ),
        })
}

/// The attributes one reported dashboard gives the resource: the kind that service records
/// it as and the two instants of its clock. A dashboard it reports nothing about gives
/// none, so the resource keeps what the naming rules computed rather than taking an empty
/// value.
fn dashboard_attributes(dashboard: Option<&Value>) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let Some(dashboard) = dashboard else {
        return attributes;
    };
    for (attribute, member) in DASHBOARD_REPORTED_ATTRIBUTES {
        if let Some(value) = reported_text(dashboard, member).filter(|text| !text.is_empty()) {
            attributes.insert((*attribute).to_string(), value);
        }
    }
    read_instants(dashboard, DASHBOARD_INSTANT_ATTRIBUTES, &mut attributes);
    attributes
}

/// The attributes one reported scheduled report gives the resource: the two instants of
/// that service's clock, read the same way a dashboard's are.
fn scheduled_report_attributes(report: Option<&Value>) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    if let Some(report) = report {
        read_instants(report, SCHEDULED_REPORT_INSTANT_ATTRIBUTES, &mut attributes);
    }
    attributes
}

/// Put every instant `pairs` names into `attributes`, as the text a published return value
/// carries one as: an awsJson timestamp is a VALUE on the wire while a CloudFormation
/// attribute is text, so the wire value is read as the instant it denotes
/// ([`instant_text`], the tree's one reading of that direction) and then written at the
/// MILLISECOND precision a published return value carries ([`published_instant`], the
/// tree's one rendering of that). A member that service does not report contributes no
/// attribute, so the resource keeps what the naming rules computed.
fn read_instants(
    reported: &Value,
    pairs: &[(&str, &str)],
    attributes: &mut BTreeMap<String, String>,
) {
    for (attribute, member) in pairs {
        let rendered = reported
            .get(*member)
            .and_then(instant_text)
            .map(|instant| published_instant(&instant));
        if let Some(instant) = rendered {
            attributes.insert((*attribute).to_string(), instant);
        }
    }
}

/// The dashboard record that service holds under `arn`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE and never on a message).
async fn read_bcm_dashboard(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    arn: &str,
) -> Result<Option<Value>, Refusal> {
    match call_bcm(ctx, region, GET_DASHBOARD, &addressed(arn)).await {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == BCM_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The scheduled-report record that service holds under `arn`, unwrapped from the
/// `scheduledReport` member its read answers one in, or `None` when it holds none.
async fn read_bcm_scheduled_report(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    arn: &str,
) -> Result<Option<Value>, Refusal> {
    match call_bcm(ctx, region, GET_SCHEDULED_REPORT, &addressed(arn)).await {
        Ok(answered) => Ok(answered.get(SCHEDULED_REPORT_MEMBER).cloned()),
        Err(refusal) if refusal.code == BCM_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Delete one resource of that service by the ARN it was created under, through the
/// `operation` that removes its kind.
async fn delete_bcm_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_bcm(ctx, region, operation, &addressed(physical_resource_id)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == BCM_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The request that addresses ONE resource of that service: the `arn` its read, its update
/// and its delete each take.
fn addressed(arn: &str) -> Value {
    Value::Object(
        [(ARN_MEMBER.to_string(), Value::String(arn.to_string()))]
            .into_iter()
            .collect(),
    )
}

/// The tag set `ListTagsForResource` reports, as the pairs the tag arithmetic takes: that
/// service answers a `resourceTags` list of structures whose members are spelled
/// `key`/`value`, which is the lower-cased spelling its creates take too.
fn held_tags(answered: &Value) -> Vec<(String, String)> {
    let Some(Value::Array(tags)) = answered.get(RESOURCE_TAGS_MEMBER) else {
        return Vec::new();
    };
    tags.iter()
        .filter_map(|tag| {
            let key = tag.get(LOWER_CASED_TAG.0).and_then(Value::as_str)?;
            if key.is_empty() {
                return None;
            }
            let value = tag
                .get(LOWER_CASED_TAG.1)
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// The declared property document of an UPDATE, with a property this arm does not carry
/// REFUSED rather than dropped — the same rule [`refuse_unmapped_properties`] applies to a
/// create.
fn refuse_unmapped(
    resource: &StoredResource,
    declared: &Value,
    mapped: &[&str],
) -> Result<Map<String, Value>, Refusal> {
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    match unmapped_property(&properties, mapped) {
        Some(property) => Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        }),
        None => Ok(properties),
    }
}

/// Call one operation of that service on the awsJson1_0 wire its model declares.
async fn call_bcm(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        BCM_DASHBOARDS_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{BCM_DASHBOARDS_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every member of either call is a property the type publishes, and every property is
    /// carried: the catalogues and the mapped sets cannot drift apart, which is what keeps
    /// a declaration from being reported as applied while a member went nowhere.
    #[test]
    fn every_mapped_property_is_carried_by_one_of_the_calls() {
        let dashboard_carried: Vec<&str> = DASHBOARD_MEMBERS
            .iter()
            .map(|(property, _)| *property)
            .chain([WIDGETS_PROPERTY, "Tags"])
            .collect();
        for property in DASHBOARD_MAPPED_PROPERTIES {
            assert!(
                dashboard_carried.contains(property),
                "case {property:?}: expected the dashboard's calls to carry a mapped \
                 property, they carry {dashboard_carried:?}"
            );
        }
        let report_carried: Vec<&str> = SCHEDULED_REPORT_MEMBERS
            .iter()
            .map(|(property, _)| *property)
            .chain([SCHEDULE_CONFIG_PROPERTY, WIDGET_DATE_RANGE_PROPERTY, "Tags"])
            .collect();
        for property in SCHEDULED_REPORT_MAPPED_PROPERTIES {
            assert!(
                report_carried.contains(property),
                "case {property:?}: expected the report's calls to carry a mapped property, \
                 they carry {report_carried:?}"
            );
        }
    }

    /// A widget's member NAMES become the API members they are, while the keys of the
    /// graph display map — which are the metrics the configuration applies to — are carried
    /// verbatim, and the numeric members reach the wire as numbers whichever spelling the
    /// template used.
    #[test]
    fn a_declared_widget_becomes_the_members_the_api_takes() {
        let declared = serde_json::json!([{
            "Title": "Spend",
            "Width": "6",
            "Height": 9,
            "HorizontalOffset": "0",
            "Configs": [{
                "QueryParameters": {"CostAndUsage": {
                    "Metrics": ["UnblendedCost"],
                    "Granularity": "MONTHLY",
                    "TimeRange": {
                        "StartTime": {"Type": "RELATIVE", "Value": "P30D"},
                        "EndTime": {"Type": "RELATIVE", "Value": "P0D"},
                    },
                }},
                "DisplayConfig": {"Graph": {"UnblendedCost": {"VisualType": "LINE"}}},
            }],
        }]);
        let widgets = declared_widgets(&declared).expect("a shallow document is carried");
        let widget = &widgets[0];
        assert_eq!(
            widget["title"].as_str(),
            Some("Spend"),
            "expected the member names converted, got {widgets}"
        );
        for (member, expected) in [("width", 6), ("height", 9), ("horizontalOffset", 0)] {
            assert_eq!(
                widget[member].as_i64(),
                Some(expected),
                "case {member:?}: expected the member on the wire as {expected}, got {widgets}"
            );
        }
        let config = &widget["configs"][0];
        assert_eq!(
            config["queryParameters"]["costAndUsage"]["granularity"].as_str(),
            Some("MONTHLY"),
            "expected the union member and its own members converted, got {widgets}"
        );
        assert_eq!(
            config["queryParameters"]["costAndUsage"]["timeRange"]["startTime"]["type"].as_str(),
            Some("RELATIVE"),
            "expected the nested structure's members converted, got {widgets}"
        );
        assert_eq!(
            config["displayConfig"]["graph"]["UnblendedCost"]["visualType"].as_str(),
            Some("LINE"),
            "expected the graph map keyed by the metric verbatim with its own members \
             converted, got {widgets}"
        );
    }

    /// A declared schedule reaches the API with its members converted and its period's two
    /// instants as the seconds that timestamp member is carried as, while an instant
    /// already stated as seconds is left alone.
    #[test]
    fn a_declared_schedule_carries_its_period_as_timestamps() {
        let declared = serde_json::json!({
            "ScheduleExpression": "cron(0 8 1 * ? *)",
            "State": "ENABLED",
            "SchedulePeriod": {"StartTime": "2026-01-01T00:00:00Z", "EndTime": 1_800_000_000},
        });
        let config = schedule_config_member(&declared).expect("a shallow document is carried");
        assert_eq!(
            config["scheduleExpression"].as_str(),
            Some("cron(0 8 1 * ? *)"),
            "expected the member names converted, got {config}"
        );
        assert_eq!(
            config["schedulePeriod"]["startTime"].as_f64(),
            Some(1_767_225_600.0),
            "expected the declared instant as epoch seconds, got {config}"
        );
        assert_eq!(
            config["schedulePeriod"]["endTime"].as_i64(),
            Some(1_800_000_000),
            "expected an instant already stated as seconds to be left alone, got {config}"
        );
    }

    /// The attributes a reported record publishes are the instants that service reported,
    /// written at the MILLISECOND precision a published return value carries — three
    /// fractional digits, trailing zeros included — and a member it does not report
    /// contributes no attribute at all.
    ///
    /// The rows differ along where the fraction sits: finer than a millisecond, exactly a
    /// whole second, and milliseconds whose last digit is a zero.
    #[test]
    fn the_published_instants_are_the_reported_ones_at_millisecond_precision() {
        let reported = serde_json::json!({
            "createdAt": 1_767_225_600.275_072_6_f64,
            "updatedAt": 1_767_225_600_i64,
        });
        let mut attributes = BTreeMap::new();
        read_instants(&reported, DASHBOARD_INSTANT_ATTRIBUTES, &mut attributes);
        assert_eq!(
            attributes.get("CreatedAt").map(String::as_str),
            Some("2026-01-01T00:00:00.275Z"),
            "expected a finer fraction published at its milliseconds, got {attributes:?}"
        );
        assert_eq!(
            attributes.get("UpdatedAt").map(String::as_str),
            Some("2026-01-01T00:00:00.000Z"),
            "expected a whole second published with its three digits, got {attributes:?}"
        );

        let rounded = serde_json::json!({"createdAt": 1_767_225_600.270_f64});
        let mut attributes = BTreeMap::new();
        read_instants(&rounded, DASHBOARD_INSTANT_ATTRIBUTES, &mut attributes);
        assert_eq!(
            attributes.get("CreatedAt").map(String::as_str),
            Some("2026-01-01T00:00:00.270Z"),
            "expected milliseconds ending in a zero to be published all the same, got \
             {attributes:?}"
        );
        assert!(
            !attributes.contains_key("UpdatedAt"),
            "a member the record does not carry contributes no attribute, got {attributes:?}"
        );

        let mut attributes = BTreeMap::new();
        read_instants(
            &serde_json::json!({"createdAt": "2026-01-01T00:00:00.412913Z"}),
            DASHBOARD_INSTANT_ATTRIBUTES,
            &mut attributes,
        );
        assert_eq!(
            attributes.get("CreatedAt").map(String::as_str),
            Some("2026-01-01T00:00:00.412Z"),
            "an instant that service wrote as text is published at its milliseconds too, got \
             {attributes:?}"
        );
    }

    /// The tag set that service reports is read in the spelling its own list carries, and
    /// an entry with no key is not a tag.
    #[test]
    fn the_reported_tag_set_is_read_in_the_services_own_spelling() {
        let answered = serde_json::json!({
            "resourceTags": [
                {"key": "team", "value": "finops"},
                {"key": "", "value": "dropped"},
                {"key": "tier"},
            ]
        });
        let held = held_tags(&answered);
        assert_eq!(
            held,
            vec![
                ("team".to_string(), "finops".to_string()),
                ("tier".to_string(), String::new()),
            ],
            "expected the reported pairs, got {held:?}"
        );
        assert!(
            held_tags(&serde_json::json!({})).is_empty(),
            "an answer carrying no tag list reports no tags"
        );
    }
}
