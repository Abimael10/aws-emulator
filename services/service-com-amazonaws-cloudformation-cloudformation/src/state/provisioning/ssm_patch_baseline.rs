// SPDX-License-Identifier: CC0-1.0

//! `AWS::SSM::PatchBaseline`: the patch baseline a stack declares, created in AWS Systems
//! Manager itself, together with the patch groups the declaration registers it for.
//!
//! This is the per-type half of provisioning for one resource type, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::iot_authorizer`] for the reason that module records —
//! the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the baseline is really created
//!
//! A patch baseline's identity is the `pb-…` id `CreatePatchBaseline` MINTS: `GetPatchBaseline`,
//! `UpdatePatchBaseline`, `DeletePatchBaseline`, `RegisterDefaultPatchBaseline` and
//! `RegisterPatchBaselineForPatchGroup` each address one by that id, and nothing else names a
//! baseline. So a stack that reported a baseline created while nothing created it publishes an
//! id that resolves to nothing — Patch Manager would patch the group's nodes against the
//! default baseline of their operating system instead, and a client's own read of the id would
//! be refused — which is what creating it where it belongs puts right.
//!
//! The PATCH GROUPS are part of the same resource: "PatchGroups: the name of the patch group to
//! be registered with the patch baseline" is a property of this type, and a group is registered
//! with a baseline by `RegisterPatchBaselineForPatchGroup` ([`register_groups`]). A baseline
//! created without them would be a baseline no node is patched against, which is the shallow
//! success this arm exists to avoid.
//!
//! ## The properties
//!
//! Every property the type publishes is a member of `CreatePatchBaseline` under the SAME name
//! ([`BASELINE_MEMBERS`]) except three, which are calls of their own: `PatchGroups` (the
//! registrations above), `DefaultBaseline` (`RegisterDefaultPatchBaseline`, which makes the
//! baseline the default for its operating system) and `Tags`, which travels as the create's own
//! tag list beside the tags CloudFormation gives the resource.
//!
//! `Name` is required by `CreatePatchBaseline` and optional on the type, so a declaration that
//! names none has one generated for it the way CloudFormation generates a name —
//! `<stack>-<logical id>-<unique>` ([`generated_baseline_name`]) — and a later update keeps
//! whatever name the baseline already has rather than inventing a second one.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::{
    applicable_tags, modelled_declaration, Created, ModelledScalar, Readiness, Refusal, Removal,
    STACK_NAME_TAG,
};
use crate::state::provisioner::{call_ssm, tags_of};
use crate::state::StoredResource;

/// The resource type this module provisions.
pub(crate) const SSM_PATCH_BASELINE: &str = "AWS::SSM::PatchBaseline";

/// The Systems Manager operations this module asks for, by name: the call that mints a
/// baseline, the one that writes one, the one that reads one back, the one that removes it,
/// the pair that registers and deregisters a patch group, and the one that makes a baseline
/// the default for its operating system.
///
/// Named rather than spelled at each call because the provider REPORTS which of them was
/// refused ([`handler_refusal`]), so the name in the report and the name in the
/// `X-Amz-Target` cannot drift apart.
const CREATE_PATCH_BASELINE: &str = "CreatePatchBaseline";
const UPDATE_PATCH_BASELINE: &str = "UpdatePatchBaseline";
const GET_PATCH_BASELINE: &str = "GetPatchBaseline";
const DELETE_PATCH_BASELINE: &str = "DeletePatchBaseline";
const REGISTER_FOR_PATCH_GROUP: &str = "RegisterPatchBaselineForPatchGroup";
const DEREGISTER_FOR_PATCH_GROUP: &str = "DeregisterPatchBaselineForPatchGroup";
const REGISTER_DEFAULT: &str = "RegisterDefaultPatchBaseline";

/// The error code Systems Manager answers for a baseline it does not hold
/// (`DoesNotExistException`, the modelled 404 of its read, its write and its registers),
/// which is what says a delete has nothing left to remove and a read has nothing to report.
const DOES_NOT_EXIST: &str = "DoesNotExistException";

/// The member that carries the id Systems Manager minted for the baseline, which is this
/// resource's physical id and what the type's `Ref` and its one published attribute resolve
/// to.
const BASELINE_ID: &str = "BaselineId";

/// The attribute `AWS::SSM::PatchBaseline` publishes through `Fn::GetAtt` ("Id: the ID of the
/// patch baseline"), which is that same minted id.
const ID_ATTRIBUTE: &str = "Id";

/// The member the baseline's NAME travels under, on the create, the write and the read.
const NAME_MEMBER: &str = "Name";

/// The member `GetPatchBaseline` reports the registered patch groups under, which is the
/// type's own `PatchGroups` property.
const PATCH_GROUPS: &str = "PatchGroups";

/// The property that asks for the baseline to be made the default for its operating system,
/// which is `RegisterDefaultPatchBaseline` rather than a member of any other call.
const DEFAULT_BASELINE: &str = "DefaultBaseline";

/// The member `RegisterPatchBaselineForPatchGroup` and its deregister take the group under.
const PATCH_GROUP: &str = "PatchGroup";

/// The member `UpdatePatchBaseline` takes to say that the declaration is the WHOLE
/// configuration: "If True, then all fields that are required by the CreatePatchBaseline
/// operation are also required for this API request. Optional fields that aren't specified
/// are set to null" (`UpdatePatchBaselineRequest$Replace`) — which is what a CloudFormation
/// update means, so a property the new declaration drops is cleared on the baseline rather
/// than left behind.
const REPLACE: &str = "Replace";

/// Every property `AWS::SSM::PatchBaseline` publishes that is a member of
/// `CreatePatchBaseline` under the same name.
///
/// Source: AWS's CloudFormation resource reference for the type against
/// `smithy_query(query="operation", operation="CreatePatchBaseline")` — every property of the
/// type is a member of that request, spelled identically, except the three that are calls of
/// their own (see the module docs). The same names are what the baseline is READ back under:
/// `GetPatchBaseline` reports each of them under its own name, which is what the drift read
/// below compares against the declaration.
const BASELINE_MEMBERS: &[&str] = &[
    "ApprovalRules",
    "ApprovedPatches",
    "ApprovedPatchesComplianceLevel",
    "ApprovedPatchesEnableNonSecurity",
    "AvailableSecurityUpdatesComplianceStatus",
    "Description",
    "GlobalFilters",
    "Name",
    "OperatingSystem",
    "RejectedPatches",
    "RejectedPatchesAction",
    "Sources",
];

/// The members `UpdatePatchBaseline` takes, which are every one of the create's except the
/// operating system: `OperatingSystem` is a member of the create and of NO update — the
/// operating system a baseline applies to is fixed when it is made — which is why the type's
/// own replacement rules name that property.
const UPDATED_MEMBERS: &[&str] = &[
    "ApprovalRules",
    "ApprovedPatches",
    "ApprovedPatchesComplianceLevel",
    "ApprovedPatchesEnableNonSecurity",
    "AvailableSecurityUpdatesComplianceStatus",
    "Description",
    "GlobalFilters",
    "Name",
    "RejectedPatches",
    "RejectedPatchesAction",
    "Sources",
];

/// Every property the type publishes. A declared property this list does not carry is one no
/// call here applies, and is refused rather than dropped.
const BASELINE_PROPERTIES: &[&str] = &[
    "ApprovalRules",
    "ApprovedPatches",
    "ApprovedPatchesComplianceLevel",
    "ApprovedPatchesEnableNonSecurity",
    "AvailableSecurityUpdatesComplianceStatus",
    "DefaultBaseline",
    "Description",
    "GlobalFilters",
    "Name",
    "OperatingSystem",
    "PatchGroups",
    "RejectedPatches",
    "RejectedPatchesAction",
    "Sources",
    "Tags",
];

/// The two members Systems Manager's own `Tag` structure spells a tag's key and value with
/// (`smithy_query(query="shape", shape="Tag")`), and the member the create's tag list stands
/// under.
const TAG_MEMBERS: (&str, &str) = ("Key", "Value");
const TAGS_MEMBER: &str = "Tags";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// How many characters the generated-name suffix carries, which is the length of the unique
/// id CloudFormation ends a name it generates with.
const GENERATED_SUFFIX_LENGTH: usize = 12;

/// How the owning model types the members of this type's documents that are NOT text: the
/// flags of a baseline and of one of its approval rules, and the number of days a rule waits
/// before it approves a patch (`PatchRule$ApproveAfterDays`).
///
/// Read through the one walk every provisioning arm whose owning model types a member as a
/// number or a boolean shares ([`modelled_declaration`]), so a template that quoted one — or
/// fed it through a `Ref` to a `String` parameter — reaches Systems Manager as the type that
/// model declares.
fn modelled_as(member: &str) -> ModelledScalar {
    match member {
        "ApprovedPatchesEnableNonSecurity" | "EnableNonSecurity" => ModelledScalar::Boolean,
        "ApproveAfterDays" => ModelledScalar::Number,
        _ => ModelledScalar::AsWritten,
    }
}

/// Create the patch baseline an `AWS::SSM::PatchBaseline` declares, in Systems Manager
/// itself, and register the patch groups the declaration asks for — so a client that deployed
/// the stack can pass `!Ref <baseline>` straight to `ssm get-patch-baseline`, and the nodes of
/// those groups are patched against this baseline rather than against the default of their
/// operating system.
///
/// The id Systems Manager minted REPLACES the stand-in the naming rules computed: it is what
/// `Ref` and the type's `Id` attribute resolve to, and what every Systems Manager call on the
/// baseline takes.
///
/// `Err` is the owning service's own refusal of the create — an approval configuration it
/// will not take, a baseline count past the account's limit — or of a register that followed
/// it, which is what the resource fails with.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, &resource.resource_type)?;
    let mut request = declared_members(&resource.properties, BASELINE_MEMBERS);
    // `Name` is required by the call and generated by CloudFormation for a declaration that
    // states none, the way it generates a name for every resource whose own name a template
    // leaves out.
    request
        .entry(NAME_MEMBER.to_string())
        .or_insert_with(|| Value::String(generated_baseline_name(resource, propagated)));
    // The baseline carries the tags its declaration asks for plus the ones CloudFormation
    // gives the resource, as `CreatePatchBaseline`'s own `Tags` list — less the keys in AWS's
    // reserved namespace, which no public call of that service writes.
    let tags = applicable_tags(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), tag_list(tags));
    }
    let answered = call_ssm(ctx, region, CREATE_PATCH_BASELINE, &Value::Object(request))
        .await
        .map_err(|refusal| handler_refusal(CREATE_PATCH_BASELINE, refusal))?;
    let baseline_id = answered_baseline_id(&answered)?;
    register_groups(
        ctx,
        region,
        &baseline_id,
        &declared_patch_groups(&resource.properties),
    )
    .await?;
    if declared_flag(resource.properties.get(DEFAULT_BASELINE)) == Some(true) {
        register_default(ctx, region, &baseline_id).await?;
    }
    let mut attributes = BTreeMap::new();
    attributes.insert(ID_ATTRIBUTE.to_string(), baseline_id.clone());
    Ok(Created {
        physical_resource_id: Some(baseline_id),
        attributes,
    })
}

/// Whether the owning service reports the created baseline ready.
///
/// Nothing is asked, and nothing waits: a baseline EXISTS from the moment
/// `CreatePatchBaseline` answers — `GetPatchBaseline` resolves it and a patch group can be
/// registered with it — and Systems Manager reports no state one settles out of.
pub(super) fn ready() -> Readiness {
    Readiness::Ready
}

/// Whether the owning service has finished removing a deleted baseline.
///
/// Nothing is asked: `DeletePatchBaseline` takes the baseline away as it answers, and Systems
/// Manager reports no state a deleted one is still being removed in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the baseline the stack already has, through
/// `UpdatePatchBaseline` — the call that takes the baseline's own id, so it keeps its
/// identity and nothing is replaced — and bring its patch-group registrations into line with
/// what the declaration now asks for.
///
/// The write carries `Replace`, so a property the new declaration DROPS is cleared on the
/// baseline rather than left behind: that is what a CloudFormation update means. `Name` is
/// required by such a write, so a declaration that states none keeps the name the baseline
/// already has rather than being given a second generated one.
///
/// `Ok(None)` is a change this call cannot make: nothing takes a baseline's DEFAULT standing
/// away — Systems Manager publishes no call that un-registers the default of an operating
/// system — so a deployment that asks for that reports the change as not applied rather than
/// as a success that dropped the caller's change.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    // The DECLARED document, not the record's: by the time this is reached the resource
    // carries the properties it was CREATED with, so a property the new declaration
    // introduces is only in `declared`.
    refuse_unapplied_properties(declared, &resource.resource_type)?;
    let was_default = declared_flag(resource.properties.get(DEFAULT_BASELINE)) == Some(true);
    let is_default = declared_flag(declared.get(DEFAULT_BASELINE)) == Some(true);
    if was_default && !is_default {
        return Ok(None);
    }
    let baseline_id = resource.physical_resource_id.clone();
    let held = read_baseline(ctx, region, &baseline_id).await?;
    let mut request = declared_members(declared, UPDATED_MEMBERS);
    request.insert(BASELINE_ID.to_string(), Value::String(baseline_id.clone()));
    request.insert(REPLACE.to_string(), Value::Bool(true));
    // The name the baseline already carries, for a declaration that states none: the write
    // requires one, and the declaration asking for no particular name is asking for the name
    // it has.
    if !request.contains_key(NAME_MEMBER) {
        if let Some(name) = held
            .as_ref()
            .and_then(|held| held.get(NAME_MEMBER))
            .filter(|name| name.is_string())
        {
            request.insert(NAME_MEMBER.to_string(), name.clone());
        }
    }
    call_ssm(ctx, region, UPDATE_PATCH_BASELINE, &Value::Object(request))
        .await
        .map_err(|refusal| handler_refusal(UPDATE_PATCH_BASELINE, refusal))?;

    // The registrations the declaration now asks for: a group it no longer names is
    // deregistered, and one it names anew is registered. Reconciled against what the owning
    // service reports the baseline holds rather than against the stored declaration, so the
    // set the baseline ends up with is the declared one whatever else touched it.
    let declared_groups = declared_patch_groups(declared);
    let registered: BTreeSet<String> = held
        .as_ref()
        .map(reported_patch_groups)
        .unwrap_or_default();
    let added: Vec<String> = declared_groups
        .iter()
        .filter(|group| !registered.contains(*group))
        .cloned()
        .collect();
    let removed: Vec<String> = registered
        .iter()
        .filter(|group| !declared_groups.contains(*group))
        .cloned()
        .collect();
    register_groups(ctx, region, &baseline_id, &added).await?;
    deregister_groups(ctx, region, &baseline_id, &removed).await?;
    if is_default && !was_default {
        register_default(ctx, region, &baseline_id).await?;
    }
    let mut attributes = BTreeMap::new();
    attributes.insert(ID_ATTRIBUTE.to_string(), baseline_id);
    Ok(Some(Created::attributes(attributes)))
}

/// Delete the baseline a stack created, addressed by the id it was created under.
///
/// The owning service refuses the delete of a baseline a patch group is still registered with
/// ("Error returned if an attempt is made to delete a patch baseline that is registered for a
/// patch group", `ResourceInUseException`), so this walks the path that service documents for
/// a teardown: every group the baseline holds is deregistered, then the baseline is deleted —
/// which is why a stack whose baseline registered a group still releases it.
///
/// A baseline the owning service no longer holds is already in the state the delete asks for,
/// judged on the CODE and never on a message.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let baseline_id = resource.physical_resource_id.clone();
    let held = read_baseline(ctx, region, &baseline_id).await?;
    let Some(held) = held else {
        return Ok(());
    };
    let registered: Vec<String> = reported_patch_groups(&held).into_iter().collect();
    deregister_groups(ctx, region, &baseline_id, &registered).await?;
    let request = serde_json::json!({ BASELINE_ID: baseline_id });
    match call_ssm(ctx, region, DELETE_PATCH_BASELINE, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == DOES_NOT_EXIST => Ok(()),
        Err(refusal) => Err(handler_refusal(DELETE_PATCH_BASELINE, refusal)),
    }
}

/// The baseline's configuration as the owning service reports it, for drift detection.
///
/// `GetPatchBaseline` answers every member the type declares its properties with under that
/// same name, so the answer maps onto the property vocabulary one for one — the registered
/// patch groups included. A baseline the owning service no longer holds was deleted
/// ([`DOES_NOT_EXIST`]).
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(held) = read_baseline(ctx, region, &resource.physical_resource_id).await? else {
        return Ok(None);
    };
    let mut configuration = Map::new();
    for property in BASELINE_MEMBERS.iter().chain([&PATCH_GROUPS]) {
        if let Some(value) = held.get(*property) {
            configuration.insert((*property).to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(configuration)))
}

/// Register `groups` with the baseline `baseline_id`, one call each — the one call that pairs
/// a patch group with a baseline.
async fn register_groups(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    baseline_id: &str,
    groups: &[String],
) -> Result<(), Refusal> {
    for group in groups {
        let request = serde_json::json!({ BASELINE_ID: baseline_id, PATCH_GROUP: group });
        call_ssm(ctx, region, REGISTER_FOR_PATCH_GROUP, &request)
            .await
            .map_err(|refusal| handler_refusal(REGISTER_FOR_PATCH_GROUP, refusal))?;
    }
    Ok(())
}

/// Take away the registrations of `groups` from the baseline `baseline_id`, one call each. A
/// baseline the owning service no longer holds has no registration left to take away.
async fn deregister_groups(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    baseline_id: &str,
    groups: &[String],
) -> Result<(), Refusal> {
    for group in groups {
        let request = serde_json::json!({ BASELINE_ID: baseline_id, PATCH_GROUP: group });
        match call_ssm(ctx, region, DEREGISTER_FOR_PATCH_GROUP, &request).await {
            Ok(_) => {}
            Err(refusal) if refusal.code == DOES_NOT_EXIST => {}
            Err(refusal) => return Err(handler_refusal(DEREGISTER_FOR_PATCH_GROUP, refusal)),
        }
    }
    Ok(())
}

/// Make the baseline `baseline_id` the default for the operating system it applies to, which
/// is what a declaration asking for `DefaultBaseline` asks for.
async fn register_default(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    baseline_id: &str,
) -> Result<(), Refusal> {
    let request = serde_json::json!({ BASELINE_ID: baseline_id });
    call_ssm(ctx, region, REGISTER_DEFAULT, &request)
        .await
        .map(|_| ())
        .map_err(|refusal| handler_refusal(REGISTER_DEFAULT, refusal))
}

/// The baseline as `GetPatchBaseline` reports it, or `None` for one the owning service no
/// longer holds.
async fn read_baseline(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    baseline_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ BASELINE_ID: baseline_id });
    match call_ssm(ctx, region, GET_PATCH_BASELINE, &request).await {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == DOES_NOT_EXIST => Ok(None),
        Err(refusal) => Err(handler_refusal(GET_PATCH_BASELINE, refusal)),
    }
}

/// The patch groups one declaration asks for, in the order it names them and without repeats:
/// registering a group twice is one registration.
///
/// Read through a SET as well as the ordered list, because the list is the REQUESTER's: a
/// declaration naming a group many times would otherwise be compared against every group read
/// so far, which is work a template could multiply.
fn declared_patch_groups(properties: &Value) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for group in properties
        .get(PATCH_GROUPS)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .filter(|group| !group.is_empty())
    {
        if seen.insert(group) {
            named.push(group.to_string());
        }
    }
    named
}

/// The patch groups `GetPatchBaseline` reports the baseline holds.
fn reported_patch_groups(held: &Value) -> BTreeSet<String> {
    held.get(PATCH_GROUPS)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The members of `members` a declaration states, as the request carries them: the property
/// and the member are the same name, so every value goes through as the template wrote it —
/// read as the TYPES the owning model declares its members as ([`modelled_as`]). A property
/// declared as `null` states no value at all, so it is left out rather than sent as a null
/// member.
fn declared_members(properties: &Value, members: &[&str]) -> Map<String, Value> {
    let mut request = Map::new();
    for member in members {
        let Some(value) = properties.get(*member).filter(|value| !value.is_null()) else {
            continue;
        };
        request.insert(
            (*member).to_string(),
            modelled_declaration(member, value, &modelled_as),
        );
    }
    request
}

/// The tag list a create carries, under the member names Systems Manager's own `Tag` spells.
fn tag_list(tags: Vec<(String, String)>) -> Value {
    Value::Array(
        tags.into_iter()
            .map(|(key, value)| {
                let mut tag = Map::new();
                tag.insert(TAG_MEMBERS.0.to_string(), Value::String(key));
                tag.insert(TAG_MEMBERS.1.to_string(), Value::String(value));
                Value::Object(tag)
            })
            .collect(),
    )
}

/// The name CloudFormation generates for a baseline whose declaration states none:
/// `<stack name>-<logical id>-<unique>`, the form it generates every unnamed resource's name
/// in, fitted to what the owning model admits (`BaselineName` is
/// `^[a-zA-Z0-9_\-.]{3,128}$`, so every other character becomes a hyphen and the name is cut
/// to that length).
///
/// The unique part is taken from the identifier the naming rules computed for this resource,
/// which is itself a function of the stack and the logical id — so the name is stable across
/// reads and across a later deployment of the same stack, and two baselines of one stack
/// never collide.
fn generated_baseline_name(resource: &StoredResource, propagated: &[(String, String)]) -> String {
    let stack = propagated
        .iter()
        .find(|(key, _)| key == STACK_NAME_TAG)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default();
    let unique: String = resource
        .physical_resource_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(GENERATED_SUFFIX_LENGTH)
        .collect();
    let name = format!("{stack}-{}-{unique}", resource.logical_id);
    fitted_name(&name)
}

/// `name` as the owning model's `BaselineName` admits it: only letters, digits, `_`, `-` and
/// `.`, between 3 and 128 characters, with anything else written as a hyphen.
fn fitted_name(name: &str) -> String {
    let mut fitted: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric()
                || character == '_'
                || character == '-'
                || character == '.'
            {
                character
            } else {
                '-'
            }
        })
        .take(MAX_BASELINE_NAME_LENGTH)
        .collect();
    while fitted.len() < MIN_BASELINE_NAME_LENGTH {
        fitted.push('0');
    }
    fitted
}

/// The bounds the owning model puts on a baseline's name (`BaselineName`, len 3..128).
const MIN_BASELINE_NAME_LENGTH: usize = 3;
const MAX_BASELINE_NAME_LENGTH: usize = 128;

/// The id Systems Manager minted for the baseline it answered with.
///
/// An answer carrying none would leave no handle a client could call that service with and
/// nothing for the stack to publish, so it is a failed create rather than a created resource.
fn answered_baseline_id(answered: &Value) -> Result<String, Refusal> {
    answered
        .get(BASELINE_ID)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Refusal {
            code: "InternalFailure".to_string(),
            message: "AWS Systems Manager answered a created patch baseline without a BaselineId"
                .to_string(),
        })
}

/// A declared boolean in either spelling a template gives one: JSON's boolean, or the text a
/// template spells one as, read case-insensitively. `None` is a property the declaration does
/// not state, or a value that is no boolean at all.
fn declared_flag(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) if text.eq_ignore_ascii_case("true") => Some(true),
        Value::String(text) if text.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a baseline from being reported as created — or as updated —
/// with less than its declaration asked for.
fn refuse_unapplied_properties(properties: &Value, resource_type: &str) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    for property in properties.keys() {
        if BASELINE_PROPERTIES.contains(&property.as_str()) {
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

/// One owning-service refusal, reported as the call that was refused: the same wording every
/// arm of this layer reports a refused handler call in
/// ([`crate::state::provisioner::handler_refusal`]).
fn handler_refusal(operation: &str, refusal: Refusal) -> Refusal {
    crate::state::provisioner::handler_refusal(operation, refusal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One deployed resource, which is all the readers below need.
    fn resource(logical_id: &str, physical_resource_id: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: logical_id.to_string(),
            resource_type: SSM_PATCH_BASELINE.to_string(),
            physical_resource_id: physical_resource_id.to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: true,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// The declared properties reach `CreatePatchBaseline` under that call's own member
    /// names, with a quoted flag and a quoted day count carried as the types the model
    /// declares — at the depth the approval rules nest them.
    #[test]
    fn the_declared_properties_are_carried_as_the_model_types_them() {
        let declared = serde_json::json!({
            "Name": "a-baseline",
            "OperatingSystem": "AMAZON_LINUX_2023",
            "ApprovedPatchesEnableNonSecurity": "true",
            "ApprovalRules": {
                "PatchRules": [{
                    "PatchFilterGroup": {
                        "PatchFilters": [{"Key": "CLASSIFICATION", "Values": ["Security"]}]
                    },
                    "ApproveAfterDays": "7",
                    "EnableNonSecurity": "false",
                    "ComplianceLevel": "CRITICAL"
                }]
            },
            "PatchGroups": ["a-group"],
            "Tags": [{"Key": "Owner", "Value": "fleet"}]
        });
        let carried = Value::Object(declared_members(&declared, BASELINE_MEMBERS));
        let expected = serde_json::json!({
            "Name": "a-baseline",
            "OperatingSystem": "AMAZON_LINUX_2023",
            "ApprovedPatchesEnableNonSecurity": true,
            "ApprovalRules": {
                "PatchRules": [{
                    "PatchFilterGroup": {
                        "PatchFilters": [{"Key": "CLASSIFICATION", "Values": ["Security"]}]
                    },
                    "ApproveAfterDays": 7,
                    "EnableNonSecurity": false,
                    "ComplianceLevel": "CRITICAL"
                }]
            }
        });
        assert_eq!(
            carried, expected,
            "case \"a full declaration\": expected {expected}, got {carried}"
        );
    }

    /// `OperatingSystem` is not a member of the update: the operating system a baseline
    /// applies to is fixed when it is made, so the write carries only what that call takes.
    #[test]
    fn the_update_carries_only_what_update_patch_baseline_takes() {
        let declared = serde_json::json!({
            "Name": "a-baseline",
            "OperatingSystem": "WINDOWS",
            "Description": "the estate baseline"
        });
        let carried = Value::Object(declared_members(&declared, UPDATED_MEMBERS));
        let expected = serde_json::json!({
            "Name": "a-baseline",
            "Description": "the estate baseline"
        });
        assert_eq!(
            carried, expected,
            "case \"a changed declaration\": expected {expected}, got {carried}"
        );
    }

    /// The patch groups a declaration asks for are read in the order it names them, without
    /// repeats, and a declaration naming none asks for no registration at all.
    #[test]
    fn the_declared_patch_groups_are_read_once_each() {
        let cases: &[(&str, Value, Vec<&str>)] = &[
            (
                "two groups",
                serde_json::json!({"PatchGroups": ["web", "db"]}),
                vec!["web", "db"],
            ),
            (
                "one group named twice",
                serde_json::json!({"PatchGroups": ["web", "web"]}),
                vec!["web"],
            ),
            ("no groups at all", serde_json::json!({}), Vec::new()),
            (
                "an empty group name",
                serde_json::json!({"PatchGroups": [""]}),
                Vec::new(),
            ),
        ];
        for (case, declared, expected) in cases {
            let read = declared_patch_groups(declared);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// A declaration that states no `Name` has one generated in the form CloudFormation
    /// generates a name in, fitted to what the owning model admits.
    #[test]
    fn an_unnamed_baseline_is_given_a_generated_name() {
        let propagated = vec![
            (STACK_NAME_TAG.to_string(), "patch estate".to_string()),
            ("Owner".to_string(), "fleet".to_string()),
        ];
        let declared = resource(
            "Baseline",
            "pb-0123456789abcdef0",
            serde_json::json!({}),
        );
        let name = generated_baseline_name(&declared, &propagated);
        assert_eq!(
            name, "patch-estate-Baseline-pb0123456789",
            "case \"a stack name carrying a space\": expected the generated name, got {name:?}"
        );
        assert!(
            name.len() >= MIN_BASELINE_NAME_LENGTH && name.len() <= MAX_BASELINE_NAME_LENGTH,
            "case \"a stack name carrying a space\": expected a name the model admits, got \
             {name:?}"
        );
    }

    /// A property no call here applies is refused, naming it; every property the type
    /// publishes is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        let declared = serde_json::json!({
            "Name": "a-baseline",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": {},
            "ApprovedPatches": [],
            "ApprovedPatchesComplianceLevel": "CRITICAL",
            "ApprovedPatchesEnableNonSecurity": false,
            "AvailableSecurityUpdatesComplianceStatus": "COMPLIANT",
            "DefaultBaseline": true,
            "Description": "a description",
            "GlobalFilters": {},
            "PatchGroups": [],
            "RejectedPatches": [],
            "RejectedPatchesAction": "ALLOW_AS_DEPENDENCY",
            "Sources": [],
            "Tags": []
        });
        assert!(
            refuse_unapplied_properties(&declared, SSM_PATCH_BASELINE).is_ok(),
            "case \"every published property\": expected it to be applied, got a refusal"
        );
        let unknown = serde_json::json!({"Name": "a-baseline", "Unheard": 1});
        let refusal = refuse_unapplied_properties(&unknown, SSM_PATCH_BASELINE)
            .expect_err("case \"a property no call carries\": expected a refusal");
        assert!(
            refusal.message.contains("Unheard"),
            "case \"a property no call carries\": expected the refusal to name it, got {:?}",
            refusal.message
        );
    }

    /// The minted id is the resource's physical id and its one published attribute; an answer
    /// carrying none is a failed create rather than a resource with no handle.
    #[test]
    fn the_minted_id_is_what_the_create_reports() {
        let answered = serde_json::json!({"BaselineId": "pb-0123456789abcdef0"});
        let id = answered_baseline_id(&answered)
            .expect("case \"an answered id\": expected the minted id");
        assert_eq!(
            id, "pb-0123456789abcdef0",
            "case \"an answered id\": expected the minted id, got {id:?}"
        );
        let silent = serde_json::json!({});
        assert!(
            answered_baseline_id(&silent).is_err(),
            "case \"an answer carrying no id\": expected a refusal"
        );
    }
}
