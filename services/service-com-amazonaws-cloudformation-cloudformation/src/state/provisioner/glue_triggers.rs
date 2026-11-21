// SPDX-License-Identifier: CC0-1.0

//! The AWS Glue TRIGGER arms: the trigger an `AWS::Glue::Trigger` declares, created in the
//! service that OWNS it. One file of [`super`]'s per-type arms, dispatched to by its
//! `Provisioner`.
//!
//! A trigger is what starts the jobs and crawlers of a Glue deployment — on a schedule, on a
//! predicate, on an event or on a caller — so a stack that declares one and creates nothing
//! leaves a deployment whose scheduling does not exist: `GetTrigger` answers
//! `EntityNotFoundException` for a resource the stack reported as created, and nothing the
//! trigger declares is ever started. It is created here through that service's own
//! `CreateTrigger`, so everything the declaration states is resolved by AWS Glue exactly as
//! it is for a client's own call — a job or a workflow it names that the account does not
//! hold, and a definition that service will not fire, are its refusals, which fail the
//! resource and roll the stack back rather than publishing a trigger nothing holds.
//!
//! The type's properties ARE `CreateTrigger`'s own members — `Type`, `Schedule`, `Actions`,
//! `Predicate`, `Description`, `StartOnCreation`, `WorkflowName` and
//! `EventBatchingCondition` are named identically on both sides (AWS's resource reference for
//! the type against `CreateTriggerRequest`) — so the resolved property document IS the
//! request, with the one member CloudFormation decides set on the way out: "Ref returns the
//! trigger name", which is this resource's physical id and the identifier every AWS Glue
//! operation addresses a trigger by.
//!
//! `Tags` is the one member not carried verbatim: the trigger carries the tags the
//! declaration asks for beside the ones CloudFormation gives the resource ([`tags_of`]), as
//! the tag MAP `CreateTrigger`'s own `Tags` member takes. That set reaches AWS Glue on the
//! CREATE alone, for the reason an `AWS::Glue::Job`'s does (`create_glue_job`): a trigger's
//! tags are that service's own `TagResource`/`UntagResource` rather than part of the
//! definition an update overwrites.

use super::*;

/// `AWS::Glue::Trigger`: "Ref returns the trigger name" — the `Name` the declaration states,
/// which is what `CreateTrigger` creates the trigger under and what every AWS Glue operation
/// on one (`GetTrigger`, `UpdateTrigger`, `StartTrigger`, `StopTrigger`, `DeleteTrigger`)
/// addresses it by. That is the physical id the naming rules already compute for a type whose
/// published return values [`template`] carries no entry for, so the type needs none there.
pub(in crate::state) const GLUE_TRIGGER: &str = "AWS::Glue::Trigger";

/// The property that is the trigger's NAME, which is its identity: every AWS Glue call about
/// one takes it as the request's own `Name`.
const GLUE_TRIGGER_NAME_PROPERTY: &str = "Name";

/// The property that is no member of `TriggerUpdate`: a trigger's tags are AWS Glue's own
/// tagging calls, not part of the definition an update overwrites.
const GLUE_TRIGGER_TAGS_PROPERTY: &str = "Tags";

/// The members `UpdateTrigger` carries the replacement definition in: the request's own
/// `TriggerUpdate` structure, and the members that structure declares
/// (`smithy_query(query="shape",shape="TriggerUpdate")`: `Name`, `Description`, `Schedule`,
/// `Actions`, `Predicate`, `EventBatchingCondition`), each spelled identically as the type's
/// own property.
///
/// `TriggerUpdate$Name` is "Reserved for future use", so it is not among them: the name a
/// trigger is held under is not something that call changes.
const GLUE_TRIGGER_UPDATE_PROPERTY: &str = "TriggerUpdate";
const GLUE_TRIGGER_UPDATE_MEMBERS: &[&str] = &[
    "Description",
    "Schedule",
    "Actions",
    "Predicate",
    "EventBatchingCondition",
];

/// The properties a trigger that goes on existing cannot take a new value for, because they
/// are members of `CreateTrigger` and of no member of `TriggerUpdate` — the type it fires on,
/// the workflow it belongs to and whether its create started it. A declaration restating one
/// of them asks for a DIFFERENT trigger, which CloudFormation replaces
/// ([`template::replacement_properties`] names this type for exactly that).
pub(in crate::state) const GLUE_TRIGGER_CREATE_ONLY_PROPERTIES: &[&str] =
    &["StartOnCreation", "Type", "WorkflowName"];

/// Create the trigger an `AWS::Glue::Trigger` declares, so a client that deployed the stack
/// and then calls `glue GetTrigger` (or `StartTrigger`) with the name the stack publishes
/// reads the trigger back rather than an `EntityNotFoundException` — and so the schedule,
/// predicate or events the declaration states really start the jobs it names.
pub(super) async fn create_glue_trigger(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut request = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    request.insert(
        GLUE_TRIGGER_NAME_PROPERTY.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if tags.is_empty() {
        request.remove(GLUE_TRIGGER_TAGS_PROPERTY);
    } else {
        request.insert(
            GLUE_TRIGGER_TAGS_PROPERTY.to_string(),
            Value::Object(tags),
        );
    }
    call_glue(ctx, region, "CreateTrigger", &Value::Object(request)).await?;
    // `CreateTrigger` answers with the name it stored, which is the physical id the stack
    // already computed, and the type publishes no `Fn::GetAtt` attribute — so there is
    // nothing the owning service decided for this resource beyond the trigger now existing.
    Ok(Created::default())
}

/// Apply a changed `AWS::Glue::Trigger` declaration to the trigger the stack created.
///
/// `UpdateTrigger` takes the whole definition — "this object updates the previous trigger
/// definition by overwriting it completely" (`TriggerUpdate`) — so the declaration this
/// deployment states IS that `TriggerUpdate`, against the `Name` the trigger is held under,
/// reduced to the members that shape declares. The three properties it carries no member for
/// are the ones a changed declaration REPLACES the trigger over
/// ([`GLUE_TRIGGER_CREATE_ONLY_PROPERTIES`]), so this arm is only ever reached for a change
/// the call can make.
pub(super) async fn update_glue_trigger(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let mut update = Map::new();
    for member in GLUE_TRIGGER_UPDATE_MEMBERS {
        if let Some(declared) = declared.get(*member) {
            update.insert((*member).to_string(), declared.clone());
        }
    }
    let request = serde_json::json!({
        GLUE_TRIGGER_NAME_PROPERTY: resource.physical_resource_id,
        GLUE_TRIGGER_UPDATE_PROPERTY: Value::Object(update),
    });
    call_glue(ctx, region, "UpdateTrigger", &request).await?;
    Ok(Created::default())
}

/// Delete the trigger a stack created, addressed by its NAME — this resource's physical id.
///
/// A trigger AWS Glue no longer holds (`EntityNotFoundException`, judged on the CODE and
/// never on a message that carries the request's own name) is already in the state the delete
/// asks for.
pub(super) async fn delete_glue_trigger(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let request = serde_json::json!({ GLUE_TRIGGER_NAME_PROPERTY: physical_resource_id });
    match call_glue(ctx, region, "DeleteTrigger", &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == GLUE_ENTITY_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The trigger as AWS Glue reports it, for drift detection: the `Trigger` document its own
/// read answers with, whose members are spelled exactly as the type's properties are. A name
/// it no longer holds a trigger under (`EntityNotFoundException`) was deleted.
pub(super) async fn describe_glue_trigger(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ GLUE_TRIGGER_NAME_PROPERTY: physical_resource_id });
    match call_glue(ctx, region, "GetTrigger", &request).await {
        Ok(answer) => Ok(Some(reported_member(&answer, "Trigger"))),
        Err(refusal) if refusal.code == GLUE_ENTITY_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}
