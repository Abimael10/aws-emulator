// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of AWS Systems Manager's Maintenance Windows: the window an
//! `AWS::SSM::MaintenanceWindow` declares and the target an
//! `AWS::SSM::MaintenanceWindowTarget` registers with one. One file of [`super`]'s per-type
//! arms, dispatched to by its `Provisioner`.
//!
//! ## What the two types are
//!
//! Neither is a NAMED resource of that service, and both carry an identifier Systems Manager
//! MINTS:
//!
//! * `CreateMaintenanceWindow` takes the window's schedule and duration and answers the
//!   `WindowId` it minted — the `mw-…` value this type's `Ref` returns and the handle
//!   `GetMaintenanceWindow`, `UpdateMaintenanceWindow` and `DeleteMaintenanceWindow` each
//!   address one by. The `Name` a declaration states is what the window is CALLED; no Systems
//!   Manager call on a window takes it.
//! * `RegisterTargetWithMaintenanceWindow` takes the window the target belongs to and the
//!   criteria it selects nodes by, and answers the `WindowTargetId` it minted — the UUID this
//!   type's `Ref` returns and the handle `DescribeMaintenanceWindowTargets` reports a target
//!   under and `DeregisterTargetFromMaintenanceWindow` removes one by.
//!
//! So each create replaces the stand-in the naming rules computed with the id that service
//! answered, which is why `template::identity_is_service_assigned` names both types
//! (`smithy_query(operation="CreateMaintenanceWindow,RegisterTargetWithMaintenanceWindow,\
//! DescribeMaintenanceWindowTargets,DeregisterTargetFromMaintenanceWindow",
//! model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/\
//! com-amazonaws-ssm-amazonssm.json")`).
//!
//! ## Why the target's own provider matters
//!
//! A target is registered WITH a window, by the id that window carries: a stack declaring both
//! resolves `{"Ref": "<the window>"}` to the `mw-…` id the create answered, and the register
//! is refused `DoesNotExistException` for any other value. A resource only RECORDED would
//! publish a target id `DescribeMaintenanceWindowTargets` reports nothing for, so a client that
//! deployed the stack and then read the window's targets back would find none.
//!
//! ## What is NOT provisioned here
//!
//! Neither type has an UPDATE arm: the one call that changes a window
//! (`UpdateMaintenanceWindow`) and the one that changes a target
//! (`UpdateMaintenanceWindowTarget`) are both still scaffold stubs of that service, each with
//! its own issue, so a changed declaration is reported as a change this emulator did not apply
//! rather than as one it dropped ([`super::Provisioner::update`]). The target's `WindowId` and
//! `ResourceType` are create-only either way — neither is a member of that update — which is
//! why `template::replacement_properties` names them.
//!
//! Neither type publishes a `Tags` property this provider applies: `CreateMaintenanceWindow`
//! takes its own `Tags` member, and the register takes none at all.

use super::*;

/// The resource types this module provisions.
pub(in crate::state) const SSM_MAINTENANCE_WINDOW: &str = "AWS::SSM::MaintenanceWindow";
pub(in crate::state) const SSM_MAINTENANCE_WINDOW_TARGET: &str =
    "AWS::SSM::MaintenanceWindowTarget";

/// The Systems Manager operations this module asks for, by name: the ones that create, read
/// and remove a maintenance window, and the ones that register, read and remove one of its
/// targets.
///
/// Named rather than spelled at each call because the provider REPORTS which of them was
/// refused ([`handler_refusal`]), so the name in the report and the name in the
/// `X-Amz-Target` cannot drift apart.
const CREATE_MAINTENANCE_WINDOW: &str = "CreateMaintenanceWindow";
const GET_MAINTENANCE_WINDOW: &str = "GetMaintenanceWindow";
const DELETE_MAINTENANCE_WINDOW: &str = "DeleteMaintenanceWindow";
const REGISTER_TARGET: &str = "RegisterTargetWithMaintenanceWindow";
const DESCRIBE_TARGETS: &str = "DescribeMaintenanceWindowTargets";
const DEREGISTER_TARGET: &str = "DeregisterTargetFromMaintenanceWindow";

/// The error code Systems Manager answers for a window or a target it does not hold
/// (`DoesNotExistException`: "the specified resource, such as a maintenance window or patch
/// baseline, doesn't exist" — the modelled refusal of every call above).
const DOES_NOT_EXIST: &str = "DoesNotExistException";

/// The members that carry an id Systems Manager minted.
const WINDOW_ID: &str = "WindowId";
const WINDOW_TARGET_ID: &str = "WindowTargetId";

/// The `AWS::SSM::MaintenanceWindow` properties `CreateMaintenanceWindow` takes under the
/// SAME member name.
///
/// Source: AWS's CloudFormation resource reference for the type against
/// `smithy_query(operation="CreateMaintenanceWindow")` — every property of the type is a
/// member of that request, spelled identically.
///
/// The same names are what the window is READ back under: `GetMaintenanceWindow` reports each
/// of them under its own name, which is what the drift read below compares against the
/// declaration.
const WINDOW_MEMBERS: &[&str] = &[
    "AllowUnassociatedTargets",
    "Cutoff",
    "Description",
    "Duration",
    "EndDate",
    "Name",
    "Schedule",
    "ScheduleOffset",
    "ScheduleTimezone",
    "StartDate",
    "Tags",
];

/// The `AWS::SSM::MaintenanceWindowTarget` properties the register takes under the SAME
/// member name, read the same way (`smithy_query(operation=
/// "RegisterTargetWithMaintenanceWindow")`): the window the target belongs to, the type of
/// target it is, the criteria it selects nodes by, and the three values a client may label it
/// with.
const TARGET_MEMBERS: &[&str] = &[
    "Description",
    "Name",
    "OwnerInformation",
    "ResourceType",
    "Targets",
    "WindowId",
];

/// Create the maintenance window an `AWS::SSM::MaintenanceWindow` declares, so a client that
/// deployed the stack and then calls `ssm get-maintenance-window` with the id the stack
/// published reads the window back rather than being refused the value as no window id at all.
///
/// The id Systems Manager minted REPLACES the stand-in the naming rules computed: it is what
/// `Ref` resolves to, and what every Systems Manager call on the window takes — including the
/// register of a target of the same stack.
pub(super) async fn create_ssm_maintenance_window(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let request = declared_request(&resource.properties, WINDOW_MEMBERS);
    let answered = call_ssm(
        ctx,
        region,
        CREATE_MAINTENANCE_WINDOW,
        &Value::Object(request),
    )
    .await
    .map_err(|refusal| handler_refusal(CREATE_MAINTENANCE_WINDOW, refusal))?;
    let window_id = answered_id(&answered, WINDOW_ID, "maintenance window")?;
    Ok(Created {
        physical_resource_id: Some(window_id),
        attributes: BTreeMap::new(),
    })
}

/// Delete the maintenance window a stack created, by the id it was created under — this
/// resource's physical id, which is what `Ref` returns for the type.
///
/// One Systems Manager no longer holds (`DOES_NOT_EXIST`, judged on the CODE and never on a
/// message that carries the request's own values) is already in the state the delete asks for.
pub(super) async fn delete_ssm_maintenance_window(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let request = serde_json::json!({ WINDOW_ID: physical_resource_id });
    match call_ssm(ctx, region, DELETE_MAINTENANCE_WINDOW, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == DOES_NOT_EXIST => Ok(()),
        Err(refusal) => Err(handler_refusal(DELETE_MAINTENANCE_WINDOW, refusal)),
    }
}

/// The window's configuration as Systems Manager reports it, for drift detection: the members
/// `GetMaintenanceWindow` answers for the id that is this resource's physical id, under the
/// property names the type declares — which are the same names. One that service no longer
/// holds was deleted, which is what `None` reports.
pub(super) async fn describe_ssm_maintenance_window(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ WINDOW_ID: physical_resource_id });
    match call_ssm(ctx, region, GET_MAINTENANCE_WINDOW, &request).await {
        Ok(answer) => {
            let reported: Vec<(&str, &str)> = WINDOW_MEMBERS
                .iter()
                .map(|member| (*member, *member))
                .collect();
            Ok(Some(reported_as_properties(&answer, &reported)))
        }
        Err(refusal) if refusal.code == DOES_NOT_EXIST => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Register the target an `AWS::SSM::MaintenanceWindowTarget` declares with the window its
/// `WindowId` names, so a client that deployed the stack and then calls
/// `ssm describe-maintenance-window-targets` for that window reads the target back.
///
/// The id Systems Manager minted for the target REPLACES the stand-in the naming rules
/// computed, the same way the window's does.
pub(super) async fn create_ssm_maintenance_window_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let request = declared_request(&resource.properties, TARGET_MEMBERS);
    let answered = call_ssm(ctx, region, REGISTER_TARGET, &Value::Object(request))
        .await
        .map_err(|refusal| handler_refusal(REGISTER_TARGET, refusal))?;
    let window_target_id = answered_id(&answered, WINDOW_TARGET_ID, "maintenance window target")?;
    Ok(Created {
        physical_resource_id: Some(window_target_id),
        attributes: BTreeMap::new(),
    })
}

/// Deregister the target a stack registered, addressed the way that service addresses one: by
/// the WINDOW it belongs to and the id the register minted for it. Only the resource's own
/// record carries both — the physical id is the target id alone — so the delete takes the
/// record.
///
/// A target that service no longer holds, and a window it no longer holds, each answer
/// `DOES_NOT_EXIST`, which is already the state the delete asks for.
pub(super) async fn delete_ssm_maintenance_window_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let Some(window_id) = declared_window_id(resource) else {
        // The window the target belongs to is the one its own declaration names, and a
        // resource whose declaration names none names no target to deregister either.
        return Ok(());
    };
    let request = serde_json::json!({
        WINDOW_ID: window_id,
        WINDOW_TARGET_ID: resource.physical_resource_id.clone(),
    });
    match call_ssm(ctx, region, DEREGISTER_TARGET, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == DOES_NOT_EXIST => Ok(()),
        Err(refusal) => Err(handler_refusal(DEREGISTER_TARGET, refusal)),
    }
}

/// The target's configuration as Systems Manager reports it, for drift detection.
///
/// That service publishes no read of ONE target, so the target is read out of its WINDOW's own
/// listing, by the id the register minted — the same position Amazon Braket's spending limits
/// and X-Ray's sampling rules are in. A window that service no longer holds, and a window that
/// holds no such target, each report the target as deleted (`None`).
pub(super) async fn describe_ssm_maintenance_window_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(window_id) = declared_window_id(resource) else {
        return Ok(None);
    };
    let request = serde_json::json!({ WINDOW_ID: window_id });
    let answered = match call_ssm(ctx, region, DESCRIBE_TARGETS, &request).await {
        Ok(answer) => answer,
        Err(refusal) if refusal.code == DOES_NOT_EXIST => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let Some(held) = answered
        .get("Targets")
        .and_then(Value::as_array)
        .and_then(|targets| {
            targets.iter().find(|target| {
                reported_text(target, WINDOW_TARGET_ID).as_deref()
                    == Some(resource.physical_resource_id.as_str())
            })
        })
    else {
        return Ok(None);
    };
    let reported: Vec<(&str, &str)> = TARGET_MEMBERS
        .iter()
        .map(|member| (*member, *member))
        .collect();
    Ok(Some(reported_as_properties(held, &reported)))
}

/// The `WindowId` a target's own declaration states: the window it was registered with, which
/// is the other half of its address.
fn declared_window_id(resource: &StoredResource) -> Option<String> {
    reported_text(&resource.properties, WINDOW_ID).filter(|id| !id.is_empty())
}

/// The request one declaration describes: the members named identically on both sides, each
/// exactly as the declaration resolved it.
///
/// The values are passed through rather than re-rendered as text: a window's `Duration` and
/// `Cutoff` are numbers, its `AllowUnassociatedTargets` a boolean and a target's `Targets` a
/// list of structures on the Systems Manager side as well as on the template's, so a value
/// re-spelled as a string would be a value that service refuses. A property declared as `null`
/// states no value at all, so it is left out rather than sent as a null member.
fn declared_request(properties: &Value, members: &[&str]) -> Map<String, Value> {
    let mut request = Map::new();
    if let Some(declared) = properties.as_object() {
        for member in members {
            match declared.get(*member) {
                Some(Value::Null) | None => continue,
                Some(value) => request.insert((*member).to_string(), value.clone()),
            };
        }
    }
    request
}

/// The id Systems Manager minted for the resource it answered with, under the member that
/// carries it.
///
/// An answer carrying none would leave no handle a client could call that service with and
/// nothing for the stack to publish, so it is a failed create rather than a created resource.
fn answered_id(answered: &Value, member: &str, what: &str) -> Result<String, Refusal> {
    let id = reported_text(answered, member).unwrap_or_default();
    if id.is_empty() {
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!("AWS Systems Manager answered a created {what} without a {member}"),
        });
    }
    Ok(id)
}
