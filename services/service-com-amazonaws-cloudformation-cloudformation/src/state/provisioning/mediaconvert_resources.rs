// SPDX-License-Identifier: CC0-1.0

//! `AWS::MediaConvert::Queue`, `AWS::MediaConvert::JobTemplate` and
//! `AWS::MediaConvert::Preset`: the transcoding estate a stack declares, created in AWS
//! Elemental MediaConvert itself.
//!
//! This is the per-type half of provisioning for three resource types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::appconfig_resources`] for the reason that module records
//! — the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the resources are really created
//!
//! Each of the three is a first-class resource of the MediaConvert control plane, addressed
//! by the NAME it was created under: `GetQueue`, `UpdateQueue` and `DeleteQueue` take a
//! queue's `Name` as an `@httpLabel`, `CreateJob` runs a job IN a queue and FROM a job
//! template, an output names a preset, and `ListQueues`/`ListJobTemplates`/`ListPresets`
//! report what the account holds
//! (`smithy_query(query="operation",operation="CreateQueue,CreateJobTemplate,CreatePreset")`
//! on that service's model). So a stack that reported any of them created while nothing
//! created it publishes, through `Ref` and its outputs, a name and an ARN that resolve to
//! nothing: the first `CreateJob` against the stack's queue is refused, every reconciling
//! read answers `NotFoundException`, and the stack's own deletion has nothing to delete. All
//! three types are therefore provisioned here rather than left declared-only.
//!
//! Their identity is CloudFormation's own to compute, not MediaConvert's to mint: "Ref
//! returns the name" for each, and that name is the `Name` the create is made under — which
//! is why none of them is named by
//! [`crate::state::template::identity_is_service_assigned`]. The ARN each `Fn::GetAtt Arn`
//! reports is the one that service answers with, which is also the one
//! [`crate::state::template::return_values`] computes for the name: both are built from the
//! one account id this emulator mints and the resource's own name, which is how MediaConvert
//! keys a queue, a job template and a preset (`queue_arn`, `job_template_arn` and
//! `preset_arn` in that service's own state module).
//!
//! ## The properties
//!
//! Every property these three types publish is a member of the owning create under the same
//! name, read in that model's own lower-camel spelling ([`modelled_member_name`]:
//! MediaConvert's members carry a `jsonName` that is the member name with a lower-case
//! initial, `PricingPlan` against `pricingPlan`, `StatusUpdateInterval` against
//! `statusUpdateInterval`). Four are not carried that way:
//!
//! * `Name` is the name CloudFormation decided for the resource — the one the declaration
//!   asked for, or the one the naming rules generated — so the create is made under it
//!   rather than under whatever the property document happens to hold;
//! * `SettingsJson` IS the `Settings` structure of the owning create ([`SETTINGS_JSON`]), so
//!   the declared document travels under that member's own name ([`SETTINGS_MEMBER`]);
//! * `Tags` is the `[{Key, Value}]` list or the map a template writes against the tag MAP
//!   MediaConvert's creates take, so it is rendered as that map ([`mediaconvert_tags`]);
//! * `SettingsJson`, `AccelerationSettings` and `HopDestinations` are STRUCTURES of the
//!   owning call, whose own members a template spells as CloudFormation spells every property
//!   — upper camel, which is how the resource and property types reference publishes a
//!   settings document (`ContainerSettings`/`Container`, `OutputGroups`,
//!   `FileGroupSettings`/`Destination`) — so their keys are read into the lower-camel spelling
//!   that model's `jsonName`s carry, at every depth, and the numbers among them out of the text
//!   a template may have quoted ([`modelled_structure`], which is the crate's own re-casing
//!   walk and the shared reading of a quoted scalar). The reading is the SAME name
//!   correspondence either way, so a document a template writes in the API's own lower-camel
//!   spelling travels unchanged — and the KEYS of the members that carry a map of CALLER data
//!   ([`DATA_KEYED_MEMBERS`]) are left exactly as the declaration wrote them, since another
//!   member of the same document names one of them by that name.
//!
//! A declared property none of those calls carries is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a resource reported created with less than its
//! declaration asked for is the shallow success these arms exist to avoid. That is also what
//! a RESERVED queue runs into — `AWS::MediaConvert::Queue` publishes no reservation-plan
//! property at all, so such a declaration reaches `CreateQueue` without the settings that
//! call requires for a reserved queue and MediaConvert refuses it, which fails the resource.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{applicable_tags, call_rest_json_named, percent_encoded, Created, Readiness, Refusal, Removal};
use crate::state::provisioner::{lower_camel_members_except_data_keys, tags_as_map, tags_of};
use crate::state::template::{
    MEDIACONVERT_JOB_TEMPLATE, MEDIACONVERT_PRESET, MEDIACONVERT_QUEUE,
};
use crate::state::StoredResource;

/// The SigV4 signing name of the service that owns these types, which is what routes the
/// in-process call to AWS Elemental MediaConvert's own handlers (that model's `sigv4Name`/
/// `endpointPrefix`, `smithy_query(query="service_info")`). It speaks `restJson1`, so every
/// call below rides [`call_rest_json_named`] on the URI its own operation's `@http` binding
/// declares.
const MEDIACONVERT_SIGNING_NAME: &str = "mediaconvert";

/// The error code MediaConvert answers for a resource it does not hold
/// (`NotFoundException`, the modelled 404 of its reads, writes and deletes), which is what
/// says a delete has nothing left to remove.
const NOT_FOUND: &str = "NotFoundException";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a create that answered 2xx without the resource it made is reported under.
const UNIDENTIFIED_FAILURE: &str = "InternalFailure";

/// Which of the three types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::MediaConvert::Queue`.
    Queue,
    /// `AWS::MediaConvert::JobTemplate`.
    JobTemplate,
    /// `AWS::MediaConvert::Preset`.
    Preset,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        MEDIACONVERT_QUEUE => Some(Kind::Queue),
        MEDIACONVERT_JOB_TEMPLATE => Some(Kind::JobTemplate),
        MEDIACONVERT_PRESET => Some(Kind::Preset),
        _ => None,
    }
}

/// The property every one of these types is created under, which carries the name
/// CloudFormation decided rather than the declaration's own text.
const NAME: &str = "Name";

/// The property carrying a resource's tags, which reaches each create as a map.
const TAGS: &str = "Tags";

/// The property carrying the transcode settings of a job template or a preset, which IS the
/// `Settings` structure of the owning create: a document written in that API's own spelling,
/// so it travels verbatim.
const SETTINGS_JSON: &str = "SettingsJson";

/// Every property `AWS::MediaConvert::Queue` publishes.
const QUEUE_PROPERTIES: &[&str] = &["Description", NAME, "PricingPlan", "Status", TAGS];

/// Every property `AWS::MediaConvert::JobTemplate` publishes.
const JOB_TEMPLATE_PROPERTIES: &[&str] = &[
    "AccelerationSettings",
    "Category",
    "Description",
    "HopDestinations",
    NAME,
    "Priority",
    "Queue",
    SETTINGS_JSON,
    "StatusUpdateInterval",
    TAGS,
];

/// Every property `AWS::MediaConvert::Preset` publishes.
const PRESET_PROPERTIES: &[&str] = &["Category", "Description", NAME, SETTINGS_JSON, TAGS];

/// The properties of a queue the owning CREATE carries as members of their own.
const QUEUE_CARRIED_PROPERTIES: &[&str] = &["Description", "PricingPlan", "Status"];

/// The properties of a job template that call carries as members of their own.
const JOB_TEMPLATE_CARRIED_PROPERTIES: &[&str] = &[
    "AccelerationSettings",
    "Category",
    "Description",
    "HopDestinations",
    "Priority",
    "Queue",
    "StatusUpdateInterval",
];

/// The properties of a preset that call carries as members of their own.
const PRESET_CARRIED_PROPERTIES: &[&str] = &["Category", "Description"];

/// The properties the owning create carries that its UPDATE does not model at all:
/// `UpdateQueue` takes a queue's description, status, concurrency and reservation plan and no
/// `pricingPlan` (`smithy_query(query="operation",operation="CreateQueue,UpdateQueue")`).
///
/// A declaration that states one DIFFERENTLY replaces the resource rather than reaching the
/// update's arm at all ([`crate::state::template::replacement_properties`] names it), so what
/// reaches that arm is a value the resource already carries — and it is left off the request,
/// since sending a member the called operation does not model is not the request a client's own
/// update would make.
const CREATE_ONLY_PROPERTIES: &[&str] = &["PricingPlan"];

/// The properties MediaConvert models as a STRUCTURE whose own members a template spells as
/// CloudFormation spells every property, so their keys and the numbers among them are read into
/// the spelling that service takes ([`modelled_structure`]).
const STRUCTURE_PROPERTIES: &[&str] = &["AccelerationSettings", "HopDestinations"];

/// The properties MediaConvert models as a NUMBER rather than as text:
/// `CreateJobTemplate$Priority` targets `__integerMinNegative50Max50`
/// (`smithy_query(query="operation",operation="CreateJobTemplate")`), so a declaration that
/// writes it as the text a parameter reference resolves to is sent as the number it spells.
const NUMBER_PROPERTIES: &[&str] = &["Priority"];

/// The members inside a declared structure MediaConvert models as a NUMBER:
/// `HopDestination$Priority` and `HopDestination$WaitMinutes`
/// (`smithy_query(query="shape",shape="HopDestination")`), read in the lower-camel spelling the
/// walk has already put them in.
const MODELLED_NUMBERS: &[&str] = &["priority", "waitMinutes"];

/// The members of a settings document that carry a MAP whose KEYS are the CALLER's own data
/// rather than modelled member names, in the lower-camel spelling the re-casing walk reaches
/// them under.
///
/// These are every map-valued member of the settings graph: `InputTemplate$AudioSelectors`,
/// `$AudioSelectorGroups`, `$CaptionSelectors` and `$DynamicAudioSelectors`, plus
/// `VideoOverlayInput$AudioSelectors`, each a `__mapOf…` in that model
/// (`smithy_query(query="shape",shape="InputTemplate")`). A caller NAMES each entry and other
/// members of the same document name one of those entries back — the `audioSourceName` of an
/// output description is one of the selector names declared here — so re-casing such a key
/// would store a document whose references no longer resolve. Their keys therefore travel
/// exactly as the declaration wrote them, while the structure inside each entry is read like
/// any other ([`lower_camel_members_except_data_keys`]).
const DATA_KEYED_MEMBERS: &[&str] = &[
    "audioSelectorGroups",
    "audioSelectors",
    "captionSelectors",
    "dynamicAudioSelectors",
];

/// How deeply a declared property document is already nested where the re-casing walk reaches
/// it: the property's value stands under the resource's own property document, so one level.
const PROPERTY_NESTING: usize = 1;

/// The state a queue the declaration no longer PAUSES is in: `QueueStatus` is `ACTIVE` or
/// `PAUSED` and `CreateQueue$Status` is the queue's "Initial state", which is `ACTIVE` where
/// the request names none — so that is the state the update puts a queue back into when the
/// declaration stops stating one.
const QUEUE_STATUS_ACTIVE: &str = "ACTIVE";

/// The `Priority` a job template created without one carries:
/// `CreateJobTemplate$Priority`'s own documentation, "if you don't specify a priority, the
/// service uses the default value 0" — so that is the value the update carries for a
/// declaration that stops stating one.
const DEFAULT_PRIORITY: i64 = 0;

/// The `StatusUpdateInterval` such a template carries: MediaConvert's own status-update cadence
/// is 60 seconds, which `StatusUpdateInterval` declares as `SECONDS_60`
/// (`smithy_query(query="shape",shape="StatusUpdateInterval")`).
const DEFAULT_STATUS_UPDATE_INTERVAL: &str = "SECONDS_60";

/// The acceleration mode such a template carries: accelerated transcoding is opt-in, and
/// `AccelerationStatus`'s own documentation names that state ("for jobs that have Acceleration
/// (AccelerationMode) set to DISABLED…"), the structure's one required member being that mode.
const ACCELERATION_MODE_DISABLED: &str = "DISABLED";

/// Create the resource one of these types declares, in AWS Elemental MediaConvert itself — so
/// a client that deployed the stack can submit a job to the stack's own queue and read every
/// resource back with that service's own reads.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, resource, properties_of(kind))?;
    let mut request = Map::new();
    request.insert(
        modelled_member_name(NAME),
        Value::String(resource.physical_resource_id.clone()),
    );
    for property in carried_properties(kind) {
        if let Some(value) = resource
            .properties
            .get(*property)
            .filter(|value| !value.is_null())
        {
            request.insert(modelled_member_name(property), modelled_value(property, value)?);
        }
    }
    carry_settings(&mut request, &resource.properties)?;
    let tags = mediaconvert_tags(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(modelled_member_name(TAGS), Value::Object(tags));
    }
    let answered = call_mediaconvert(
        ctx,
        region,
        create_operation(kind),
        "POST",
        collection_uri(kind),
        Some(&Value::Object(request)),
    )
    .await?;
    created_resource(&answered, kind)
}

/// Whether the owning service reports the created resource ready.
///
/// Nothing is asked, and nothing waits: each of these resources EXISTS from the moment its
/// create answers — a queue can take jobs, a job template can be created from, a preset can
/// be named by an output — and a queue is answered in the `Status` its declaration asked for,
/// which is a setting of the queue rather than a state it settles out of. None of the three
/// shapes publishes a state a brand-new resource is still coming up in
/// (`smithy_query(query="shape",shape="Queue,JobTemplate,Preset")`).
pub(super) fn ready() -> Readiness {
    Readiness::Ready
}

/// Whether the owning service has finished removing a deleted resource.
///
/// Nothing is asked: each of `DeleteQueue`, `DeleteJobTemplate` and `DeletePreset` takes the
/// resource away as it answers, and MediaConvert reports no state a removed one is still
/// being deleted in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the resource the stack already has, through the owning
/// service's own update call.
///
/// MediaConvert publishes one per type, each addressing the resource by the name it was
/// created under and taking the members that CAN change on the resource that exists — so the
/// resource keeps its identity and the declaration is applied to it. A member the declaration
/// no longer states is carried as the value a resource created WITHOUT it holds
/// ([`cleared_member`]), which is what takes a description or a category away, puts a queue
/// back into the state it is created in, and returns a job template's priority, status-update
/// cadence, acceleration settings and hop destinations to the values that service applies.
///
/// `Ok(None)` is a change no update call can make: a member the declaration DROPS that the
/// owning API publishes no way to clear — `UpdateJobTemplate$Queue` is "the new queue for the
/// job template, if you are changing it" and MediaConvert refuses an empty one, so a template
/// that stops naming a queue cannot be moved back to the default queue by any call. Such a
/// deployment reports the change as not applied rather than as a success that dropped the
/// caller's change.
///
/// A member neither update carries at all is one of the properties CloudFormation REPLACES a
/// resource over ([`crate::state::template::replacement_properties`] names a queue's pricing
/// plan, which `UpdateQueue` has no member for, and the name a resource is created under is
/// in that position for every type here), so a deployment that restates one replaces the
/// resource rather than reaching this arm.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, resource, properties_of(kind))?;
    let mut request = Map::new();
    for property in carried_properties(kind) {
        // A member only the create models is never sent on an update ([`CREATE_ONLY_PROPERTIES`]).
        if CREATE_ONLY_PROPERTIES.contains(property) {
            continue;
        }
        match declared.get(*property) {
            Some(value) if !value.is_null() => {
                request.insert(modelled_member_name(property), modelled_value(property, value)?);
            }
            // Stated before and no longer: the member carries the value a resource created
            // without it holds, so what the resource holds matches the declaration in force.
            _ if resource
                .properties
                .get(*property)
                .is_some_and(|held| !held.is_null()) =>
            {
                match cleared_member(property) {
                    Some(cleared) => {
                        request.insert(modelled_member_name(property), cleared);
                    }
                    None => return Ok(None),
                }
            }
            _ => {}
        }
    }
    carry_settings(&mut request, declared)?;
    if request.is_empty() {
        // Nothing this call carries is stated differently, so the resource is already as the
        // declaration asks: it keeps its identity and no call is made about it.
        return Ok(Some(Created::attributes(BTreeMap::new())));
    }
    let answered = call_mediaconvert(
        ctx,
        region,
        update_operation(kind),
        "PUT",
        &addressed(kind, &resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    created_resource(&answered, kind).map(Some)
}

/// Delete the resource a stack created.
///
/// Each delete addresses the resource by the name it was created under — that operation's one
/// `@httpLabel`, the same one its read binds. A resource the owning service no longer holds is
/// already in the state the delete asks for, judged on the CODE and never on a message.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let uri = addressed(kind, &resource.physical_resource_id);
    match call_mediaconvert(ctx, region, delete_operation(kind), "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Every property the type of `kind` publishes, for the one check that refuses a declared
/// property no call this module makes carries.
fn properties_of(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Queue => QUEUE_PROPERTIES,
        Kind::JobTemplate => JOB_TEMPLATE_PROPERTIES,
        Kind::Preset => PRESET_PROPERTIES,
    }
}

/// Every property of `kind` the owning CREATE carries as a member of its own, in the order the
/// type's reference publishes them: every property it publishes except the ones carried by a
/// rule of their own — the NAME the resource is created under, the TAGS map and the SETTINGS
/// document. The UPDATE carries the same set less [`CREATE_ONLY_PROPERTIES`].
fn carried_properties(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Queue => QUEUE_CARRIED_PROPERTIES,
        Kind::JobTemplate => JOB_TEMPLATE_CARRIED_PROPERTIES,
        Kind::Preset => PRESET_CARRIED_PROPERTIES,
    }
}

/// The collection every resource of `kind` lives in, which that type's create is bound to:
/// the `@http` URI of `CreateQueue`, `CreateJobTemplate` and `CreatePreset`.
fn collection_uri(kind: Kind) -> &'static str {
    match kind {
        Kind::Queue => "/2017-08-29/queues",
        Kind::JobTemplate => "/2017-08-29/jobTemplates",
        Kind::Preset => "/2017-08-29/presets",
    }
}

/// The URI that addresses ONE resource of `kind`, for the calls that read, change or delete
/// it — the one place each type's addressing is written, so a delete and an update cannot come
/// to address different resources.
fn addressed(kind: Kind, name: &str) -> String {
    format!("{}/{}", collection_uri(kind), percent_encoded(name))
}

/// The create of `kind`, as that operation's own model names it — which is part of the
/// diagnostic MediaConvert's SDK words on a refused call ([`call_rest_json_named`]).
fn create_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::Queue => "CreateQueue",
        Kind::JobTemplate => "CreateJobTemplate",
        Kind::Preset => "CreatePreset",
    }
}

/// The update of `kind`. See [`create_operation`].
fn update_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::Queue => "UpdateQueue",
        Kind::JobTemplate => "UpdateJobTemplate",
        Kind::Preset => "UpdatePreset",
    }
}

/// The delete of `kind`. See [`create_operation`].
fn delete_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::Queue => "DeleteQueue",
        Kind::JobTemplate => "DeleteJobTemplate",
        Kind::Preset => "DeletePreset",
    }
}

/// The member of a create's or an update's ANSWER that carries the resource it acted on:
/// `CreateQueue` answers `queue`, `CreateJobTemplate` `jobTemplate` and `CreatePreset`
/// `preset` (each operation's output member, in its model's own `jsonName` spelling).
fn answered_member(kind: Kind) -> &'static str {
    match kind {
        Kind::Queue => "queue",
        Kind::JobTemplate => "jobTemplate",
        Kind::Preset => "preset",
    }
}

/// What a create or an update answered: the ARN the owning service reports for the resource,
/// which is what this type's `Fn::GetAtt Arn` carries.
///
/// A call that answered 2xx WITHOUT the resource identifies nothing, so the resource is
/// reported as failed rather than recorded as created on the strength of an empty answer.
fn created_resource(answered: &Value, kind: Kind) -> Result<Created, Refusal> {
    let member = answered_member(kind);
    let arn = answered
        .get(member)
        .and_then(|resource| resource.get("arn"))
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
        .ok_or_else(|| Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!("MediaConvert answered without the {member}'s arn"),
        })?;
    let mut attributes = BTreeMap::new();
    attributes.insert(ARN_ATTRIBUTE.to_string(), arn.to_string());
    Ok(Created::attributes(attributes))
}

/// The attribute each of these types publishes the resource's ARN under: "Arn — the Amazon
/// Resource Name (ARN) of the queue", and the same for a job template and a preset.
const ARN_ATTRIBUTE: &str = "Arn";

/// What a member carries for a declaration that no longer states its property: the value a
/// resource created WITHOUT that property holds, which is what brings the resource into step
/// with the declaration in force.
///
/// A description and a category are cleared with the empty string — each is "optional" free
/// text, which MediaConvert holds as absent when the value carries no characters — a queue's
/// status with the state a queue that nothing paused is in ([`QUEUE_STATUS_ACTIVE`]), and a job
/// template's priority, status-update cadence, acceleration settings and hop destinations with
/// the values that service applies to a template that states none ([`DEFAULT_PRIORITY`],
/// [`DEFAULT_STATUS_UPDATE_INTERVAL`], [`ACCELERATION_MODE_DISABLED`], and the empty list of
/// "optional list of hop destinations").
///
/// `None` is a member the owning API publishes no way to clear: `UpdateJobTemplate$Queue` is
/// "the new queue for the job template, if you are changing it" and MediaConvert refuses an
/// empty one, so no call moves a template that stops naming a queue back to the default queue.
/// Such a change is reported as not applied ([`update`]) rather than left silently undone.
fn cleared_member(property: &str) -> Option<Value> {
    match property {
        "Description" | "Category" => Some(Value::String(String::new())),
        "Status" => Some(Value::String(QUEUE_STATUS_ACTIVE.to_string())),
        "Priority" => Some(Value::Number(DEFAULT_PRIORITY.into())),
        "StatusUpdateInterval" => {
            Some(Value::String(DEFAULT_STATUS_UPDATE_INTERVAL.to_string()))
        }
        "HopDestinations" => Some(Value::Array(Vec::new())),
        "AccelerationSettings" => Some(serde_json::json!({
            "mode": ACCELERATION_MODE_DISABLED,
        })),
        _ => None,
    }
}

/// One declared property's value as the owning call takes it: a STRUCTURE read into the
/// spelling that model's members carry ([`modelled_structure`]), a NUMBER read out of the text
/// a template may have quoted it as, and anything else exactly as the declaration wrote it —
/// so that service's own validation judges what it would judge for a client's own call.
fn modelled_value(property: &str, value: &Value) -> Result<Value, Refusal> {
    if STRUCTURE_PROPERTIES.contains(&property) {
        return modelled_structure(value);
    }
    if NUMBER_PROPERTIES.contains(&property) {
        return Ok(super::modelled_declaration(
            &modelled_member_name(property),
            value,
            &modelled_scalar,
        ));
    }
    Ok(value.clone())
}

/// Put the transcode settings a job template or a preset declares on the request, as the
/// `Settings` member of the owning call.
///
/// The document is one more declared STRUCTURE of that call, so its member keys are read into
/// the spelling MediaConvert takes exactly as every other structure's are
/// ([`modelled_structure`]) — a declaration a template writes in CloudFormation's own upper
/// camel and one written in the API's lower camel therefore reach the same members. A type
/// whose declaration does not carry the property sends none and nothing is inserted.
fn carry_settings(request: &mut Map<String, Value>, properties: &Value) -> Result<(), Refusal> {
    match properties.get(SETTINGS_JSON) {
        Some(Value::Null) | None => {}
        Some(settings) => {
            request.insert(SETTINGS_MEMBER.to_string(), modelled_structure(settings)?);
        }
    }
    Ok(())
}

/// The member of the owning create and update that carries those settings: `Settings` in that
/// model, `settings` on its wire.
const SETTINGS_MEMBER: &str = "settings";

/// One declared STRUCTURE as the owning call takes it: its member keys in the lower-camel
/// spelling that model's `jsonName`s carry, with the keys of the members that hold a map of
/// CALLER data left as the declaration wrote them
/// ([`lower_camel_members_except_data_keys`], the crate's own re-casing walk, which refuses a
/// document nested deeper than it carries rather than truncating it), and the numbers among
/// them read out of the text a template may have quoted them as
/// ([`super::modelled_declaration`], the one reading of a quoted scalar every provisioning arm
/// in this position shares).
fn modelled_structure(value: &Value) -> Result<Value, Refusal> {
    let modelled =
        lower_camel_members_except_data_keys(value, PROPERTY_NESTING, DATA_KEYED_MEMBERS)?;
    Ok(super::modelled_declaration("", &modelled, &modelled_scalar))
}

/// What the owning model types one member as, where that is not text ([`MODELLED_NUMBERS`]).
fn modelled_scalar(member: &str) -> super::ModelledScalar {
    if MODELLED_NUMBERS.contains(&member) {
        super::ModelledScalar::Number
    } else {
        super::ModelledScalar::AsWritten
    }
}

/// One property name as the member name MediaConvert's wire spells it with: the model's own
/// member name with a LOWER-CASE initial, which is the `jsonName` every member of that model
/// carries (`Name`/`name`, `PricingPlan`/`pricingPlan`, `ReservedSlots`/`reservedSlots`).
///
/// The one reading of that correspondence for this module's own TOP-LEVEL members, as the
/// sibling arms in this position carry one ([`super::appsync_resources`]); the document walk
/// inside a declared structure is the crate's shared one.
fn modelled_member_name(property: &str) -> String {
    let mut characters = property.chars();
    match characters.next() {
        Some(initial) => initial.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// Those of the tags CloudFormation gives one of these resources that MediaConvert's own
/// creates will TAKE, as the map they take them in.
///
/// The resource carries the tags its declaration asks for plus the ones CloudFormation
/// propagates, less the keys in AWS's reserved namespace: the three tags CloudFormation
/// records of its own accord have no public call of that service to set them through, so they
/// are left off rather than sent, since a deployment must not fail over tags CloudFormation
/// adds ([`applicable_tags`], the one place that rule is written).
fn mediaconvert_tags(properties: &Value, propagated: &[(String, String)]) -> Map<String, Value> {
    tags_as_map(&tags_of(properties, &applicable_tags(propagated)))
}

/// The refusal for a declared property no call this module makes carries, naming which
/// property it was: refusing is what keeps a resource from being reported as created — or as
/// updated — with less than its declaration asked for. `properties` is the document the
/// deployment DECLARES, which on an update is not the one the record carries.
fn refuse_unapplied_properties(
    properties: &Value,
    resource: &StoredResource,
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
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    Ok(())
}

/// Call one AWS Elemental MediaConvert operation on the `restJson1` wire its model declares,
/// on the URI that operation's own `@http` binding states, naming the operation so a refusal
/// carries the diagnostic that service's own SDK would word.
async fn call_mediaconvert(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    method: &'static str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json_named(
        ctx,
        MEDIACONVERT_SIGNING_NAME,
        region,
        operation,
        method,
        uri,
        body,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `StoredResource` of one of these types, as a deployment hands it to the arms above.
    fn declared(resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "Subject".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: "a-name".to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// The three type names this module owns are exactly the ones it claims, and nothing else:
    /// a near miss of one is another type's, which this module must not answer for.
    #[test]
    fn the_owned_types_are_the_three_mediaconvert_types() {
        let cases: &[(&str, Option<Kind>)] = &[
            (MEDIACONVERT_QUEUE, Some(Kind::Queue)),
            (MEDIACONVERT_JOB_TEMPLATE, Some(Kind::JobTemplate)),
            (MEDIACONVERT_PRESET, Some(Kind::Preset)),
            ("AWS::MediaConvert::Job", None),
            ("AWS::MediaLive::Channel", None),
            ("AWS::SQS::Queue", None),
            ("", None),
        ];
        for (resource_type, expected) in cases {
            let actual = kind_of(resource_type);
            assert_eq!(
                actual, *expected,
                "case {resource_type:?}: expected kind_of {expected:?}, got {actual:?}"
            );
        }
    }

    /// Each type's update and delete address the resource exactly as its own operation's
    /// `@http` binding does: the collection the create is bound to, then the name as one URI
    /// segment.
    #[test]
    fn a_resource_is_addressed_as_its_own_operation_binds_it() {
        let cases: &[(&str, Kind, &str, &str)] = &[
            ("a queue", Kind::Queue, "q-1", "/2017-08-29/queues/q-1"),
            (
                "a job template",
                Kind::JobTemplate,
                "jt-1",
                "/2017-08-29/jobTemplates/jt-1",
            ),
            ("a preset", Kind::Preset, "p-1", "/2017-08-29/presets/p-1"),
            (
                "a name carrying a character a URI gives meaning to",
                Kind::Queue,
                "a/b c",
                "/2017-08-29/queues/a%2Fb%20c",
            ),
        ];
        for (case, kind, name, expected) in cases {
            let actual = addressed(*kind, name);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the URI {expected:?}, got {actual:?}"
            );
        }
    }

    /// A property no call this module makes carries is refused, naming it, so nothing is
    /// reported created with less than the declaration asked for — and every property each
    /// type publishes is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        let resource = declared(MEDIACONVERT_QUEUE, Value::Null);
        let published = serde_json::json!({
            "Name": "a-queue",
            "Description": "the queue",
            "PricingPlan": "ON_DEMAND",
            "Status": "ACTIVE",
            "Tags": { "team": "video" }
        });
        assert!(
            refuse_unapplied_properties(&published, &resource, QUEUE_PROPERTIES).is_ok(),
            "case \"every published property\": expected it to be applied, got a refusal"
        );
        // A reserved queue's plan is no property of this type at all, which is why a RESERVED
        // declaration reaches `CreateQueue` without one.
        let unknown =
            serde_json::json!({ "Name": "a-queue", "ReservationPlanSettings": { "Commitment": "ONE_YEAR" } });
        let refusal = refuse_unapplied_properties(&unknown, &resource, QUEUE_PROPERTIES)
            .expect_err("case \"a property no call carries\": expected a refusal");
        assert!(
            refusal.message.contains("ReservationPlanSettings"),
            "case \"a property no call carries\": expected the refusal to name it, got {:?}",
            refusal.message
        );
    }

    /// RULE: a declared property reaches the owning call under that model's own member name —
    /// the property name with a lower-case initial — at every depth the document nests it, and
    /// the members the model types as numbers are read out of the text a template may have
    /// quoted them as.
    #[test]
    fn a_declared_structure_travels_in_the_models_own_spelling() {
        let cases: &[(&str, Value, Value)] = &[
            (
                "an acceleration mode",
                serde_json::json!({ "Mode": "PREFERRED" }),
                serde_json::json!({ "mode": "PREFERRED" }),
            ),
            (
                "a hop destination whose numbers the template quoted",
                serde_json::json!([{ "Priority": "10", "Queue": "arn", "WaitMinutes": "4" }]),
                serde_json::json!([{ "priority": 10, "queue": "arn", "waitMinutes": 4 }]),
            ),
            (
                "a hop destination whose numbers are JSON's own",
                serde_json::json!([{ "Priority": -50, "WaitMinutes": 1 }]),
                serde_json::json!([{ "priority": -50, "waitMinutes": 1 }]),
            ),
            (
                "text that names no number at all",
                serde_json::json!([{ "WaitMinutes": "soon" }]),
                serde_json::json!([{ "waitMinutes": "soon" }]),
            ),
        ];
        for (case, declared, expected) in cases {
            let actual = modelled_structure(declared)
                .unwrap_or_else(|refusal| panic!("case {case:?}: expected a value, got {refusal}"));
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// RULE: a declared settings document reaches the owning call's `Settings` member under
    /// that model's own member names, whichever spelling the template wrote them in — so
    /// CloudFormation's own upper camel and the API's lower camel reach the same members — while
    /// the KEYS of a member that carries a map of CALLER data travel exactly as the declaration
    /// wrote them, since another member of the same document names one of them back.
    #[test]
    fn the_settings_document_travels_in_the_models_own_spelling() {
        let cases: &[(&str, Value, Value)] = &[
            (
                "a document in CloudFormation's own spelling",
                serde_json::json!({
                    "OutputGroups": [{
                        "Name": "File Group",
                        "Outputs": [{ "ContainerSettings": { "Container": "MP4" } }]
                    }]
                }),
                serde_json::json!({
                    "outputGroups": [{
                        "name": "File Group",
                        "outputs": [{ "containerSettings": { "container": "MP4" } }]
                    }]
                }),
            ),
            (
                "a document already in the API's own spelling",
                serde_json::json!({ "containerSettings": { "container": "M3U8" } }),
                serde_json::json!({ "containerSettings": { "container": "M3U8" } }),
            ),
            (
                "a member whose name is an initialism",
                serde_json::json!({ "VideoDescription": { "Width": 1280 } }),
                serde_json::json!({ "videoDescription": { "width": 1280 } }),
            ),
            // Every one of the maps whose keys are the caller's own, in both spellings of the
            // member that carries them: the entry's KEY is left as declared and the structure
            // inside it is read like any other, so the `audioSourceName` that names a selector
            // back still resolves.
            (
                "the caller's own audio-selector names, in CloudFormation's spelling",
                serde_json::json!({
                    "Inputs": [{
                        "AudioSelectors": {
                            "Audio Selector 1": { "DefaultSelection": "DEFAULT" }
                        },
                        "AudioSelectorGroups": {
                            "Audio Group 1": { "AudioSelectorNames": ["Audio Selector 1"] }
                        },
                        "CaptionSelectors": {
                            "Captions Selector 1": { "LanguageCode": "ENG" }
                        },
                        "DynamicAudioSelectors": {
                            "Dynamic Audio 1": { "SelectorType": "ALL_TRACKS" }
                        }
                    }]
                }),
                serde_json::json!({
                    "inputs": [{
                        "audioSelectors": {
                            "Audio Selector 1": { "defaultSelection": "DEFAULT" }
                        },
                        "audioSelectorGroups": {
                            "Audio Group 1": { "audioSelectorNames": ["Audio Selector 1"] }
                        },
                        "captionSelectors": {
                            "Captions Selector 1": { "languageCode": "ENG" }
                        },
                        "dynamicAudioSelectors": {
                            "Dynamic Audio 1": { "selectorType": "ALL_TRACKS" }
                        }
                    }]
                }),
            ),
            (
                "the caller's own audio-selector names, in the API's spelling",
                serde_json::json!({
                    "inputs": [{
                        "audioSelectors": { "Audio Selector 1": { "defaultSelection": "DEFAULT" } }
                    }],
                    "audioDescriptions": [{ "audioSourceName": "Audio Selector 1" }]
                }),
                serde_json::json!({
                    "inputs": [{
                        "audioSelectors": { "Audio Selector 1": { "defaultSelection": "DEFAULT" } }
                    }],
                    "audioDescriptions": [{ "audioSourceName": "Audio Selector 1" }]
                }),
            ),
            (
                "a video overlay's own audio selectors, the same member one level further in",
                serde_json::json!({
                    "Inputs": [{
                        "VideoOverlays": [{
                            "AudioSelectors": { "Overlay Audio": { "DefaultSelection": "DEFAULT" } }
                        }]
                    }]
                }),
                serde_json::json!({
                    "inputs": [{
                        "videoOverlays": [{
                            "audioSelectors": { "Overlay Audio": { "defaultSelection": "DEFAULT" } }
                        }]
                    }]
                }),
            ),
        ];
        for (case, declared, expected) in cases {
            let mut request = Map::new();
            carry_settings(
                &mut request,
                &serde_json::json!({ SETTINGS_JSON: declared.clone() }),
            )
            .unwrap_or_else(|refusal| panic!("case {case:?}: expected a request, got {refusal}"));
            assert_eq!(
                request.get(SETTINGS_MEMBER),
                Some(expected),
                "case {case:?}: expected {expected}, got {request:?}"
            );
        }
    }

    /// A member a declaration DROPS carries the value a resource created without it holds, and
    /// the one member the owning API publishes no way to clear carries none — which is what
    /// makes such a change report as not applied rather than as a success that dropped it.
    #[test]
    fn a_dropped_member_carries_the_value_a_resource_without_it_holds() {
        let cases: &[(&str, Option<Value>)] = &[
            ("Description", Some(serde_json::json!(""))),
            ("Category", Some(serde_json::json!(""))),
            ("Status", Some(serde_json::json!("ACTIVE"))),
            ("Priority", Some(serde_json::json!(0))),
            ("StatusUpdateInterval", Some(serde_json::json!("SECONDS_60"))),
            ("HopDestinations", Some(serde_json::json!([]))),
            (
                "AccelerationSettings",
                Some(serde_json::json!({ "mode": "DISABLED" })),
            ),
            ("Queue", None),
        ];
        for (property, expected) in cases {
            let actual = cleared_member(property);
            assert_eq!(
                actual.as_ref(),
                expected.as_ref(),
                "case {property:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a member only the owning CREATE models is carried by that create and left off
    /// every update — and it is one of the properties CloudFormation replaces the resource
    /// over, so a declaration that states it DIFFERENTLY never reaches the update's arm at all.
    #[test]
    fn a_create_only_member_is_carried_by_the_create_alone() {
        for property in CREATE_ONLY_PROPERTIES {
            let carried = [Kind::Queue, Kind::JobTemplate, Kind::Preset]
                .into_iter()
                .any(|kind| carried_properties(kind).contains(property));
            assert!(
                carried,
                "case {property:?}: expected the create of one of these types to carry it"
            );
            let replaces = [
                MEDIACONVERT_QUEUE,
                MEDIACONVERT_JOB_TEMPLATE,
                MEDIACONVERT_PRESET,
            ]
            .into_iter()
            .any(|resource_type| {
                crate::state::template::replacement_properties(resource_type).contains(property)
            });
            assert!(
                replaces,
                "case {property:?}: expected a changed declaration to replace the resource, \
                 which replacement_properties is what says"
            );
        }
    }
}
