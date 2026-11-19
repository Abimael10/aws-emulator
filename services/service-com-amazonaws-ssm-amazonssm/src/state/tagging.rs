// SPDX-License-Identifier: CC0-1.0

//! The tag plane: the one tag set each resource this store holds carries, and how a
//! request addresses it.
//!
//! Tagging is a single rule that spans resource types. A request names a resource by
//! `(ResourceType, ResourceId)` — "Use the ID of the resource", which for `Document` and
//! `Parameter` is "the name of the resource", for `ManagedInstance` is the `mi-ID_number`
//! node id (`AddTagsToResourceRequest$ResourceId`) and for an `OpsItem`, a
//! `CloudConnector`, an `Association` or a `MaintenanceWindow` is the id its create minted,
//! and for an `OpsMetadata` object is the ARN its create minted
//! — and that pair
//! resolves to the ONE record the Region holds for it. Each such record owns its tag
//! set, so a tag written through `AddTagsToResource` is the tag `ListTagsForResource`
//! reports and the tag `RemoveTagsFromResource` drops, and a resource's tags live with
//! the resource rather than in a second store beside it. A record is reached through
//! [`ComAmazonawsSsmAmazonssmState::resource_tags`] and
//! [`ComAmazonawsSsmAmazonssmState::resource_tags_mut`]; a pair that resolves to none is
//! what the tag operations refuse as an invalid resource id.
//!
//! "Adds or overwrites one or more tags" (`AddTagsToResource`): a key the resource
//! already carries takes the submitted value rather than appearing twice, which is what
//! makes the set a map from key to value ([`merge_tags`]).
//!
//! The pair is read in two steps, and each step has its own refusal: the id is first read
//! against the form the DECLARED type's ids are published in
//! ([`resource_id_format_violation`]) — an id of another type's form names no resource of
//! this type whatever the Region holds, so that is a refusal of the request — and only then
//! resolved against the records the Region holds, where a well-formed id naming nothing is
//! the messageless `InvalidResourceId`.

use std::collections::BTreeMap;

use crate::ops::types::Tag;

use super::ComAmazonawsSsmAmazonssmState;

/// The `ResourceTypeForTagging` value whose id is a Parameter Store parameter's name
/// (from the modelled enum, `crate::ops::types::RESOURCE_TYPE_FOR_TAGGING_VALUES`).
pub const TAGGING_TYPE_PARAMETER: &str = "Parameter";

/// The `ResourceTypeForTagging` value whose id is an SSM document's name (same enum).
pub const TAGGING_TYPE_DOCUMENT: &str = "Document";

/// The `ResourceTypeForTagging` value whose id is a managed node's id (same enum).
pub const TAGGING_TYPE_MANAGED_INSTANCE: &str = "ManagedInstance";

/// The `ResourceTypeForTagging` value whose id is an OpsItem's own id (same enum).
pub const TAGGING_TYPE_OPS_ITEM: &str = "OpsItem";

/// The `ResourceTypeForTagging` value whose id is an OpsMetadata object's ARN (same enum):
/// an OpsMetadata object is addressed by the ARN its create minted and by nothing else, so
/// that ARN is the "ID of the resource" a tag request names it by.
pub const TAGGING_TYPE_OPS_METADATA: &str = "OpsMetadata";

/// The `ResourceTypeForTagging` value whose id is a cloud connector's minted id
/// (`super::cloud_connectors::CLOUD_CONNECTOR_RESOURCE_TYPE`, from the same enum).
pub const TAGGING_TYPE_CLOUD_CONNECTOR: &str = super::cloud_connectors::CLOUD_CONNECTOR_RESOURCE_TYPE;

/// The `ResourceTypeForTagging` value whose id is an automation execution's own id —
/// "Automation: example-c160-4567-8519-012345abcde" (`AddTagsToResourceRequest$ResourceId`),
/// which is the id `StartAutomationExecution` minted (same enum).
pub const TAGGING_TYPE_AUTOMATION: &str = "Automation";

/// The `ResourceTypeForTagging` value whose id is a State Manager association's minted id
/// (`super::associations::TAGGING_TYPE_ASSOCIATION`, from the same enum): "Adds or
/// overwrites one or more tags for a State Manager association"
/// (`CreateAssociationRequest$Tags`), so the tag set a create wrote is the one the tag
/// operations read and rewrite.
pub const TAGGING_TYPE_ASSOCIATION: &str = super::associations::TAGGING_TYPE_ASSOCIATION;

/// The `ResourceTypeForTagging` value whose id is a Patch Manager baseline's minted id:
/// "PatchBaseline: pb-012345abcde" (`AddTagsToResourceRequest$ResourceId`), from the same
/// enum.
pub const TAGGING_TYPE_PATCH_BASELINE: &str = "PatchBaseline";

/// The `ResourceTypeForTagging` value whose id is a maintenance window's minted id
/// (`super::maintenance_windows::TAGGING_TYPE_MAINTENANCE_WINDOW`, from the same enum):
/// "MaintenanceWindow: mw-012345abcde" (`AddTagsToResourceRequest$ResourceId`), so the
/// tag set a create wrote is the one the tag operations read and rewrite.
pub const TAGGING_TYPE_MAINTENANCE_WINDOW: &str =
    super::maintenance_windows::TAGGING_TYPE_MAINTENANCE_WINDOW;

/// How many tags one resource may carry: "Most resources can have a maximum of 50 tags"
/// (`AddTagsToResource`'s own doc). The same doc states one exception — "Automations can
/// have a maximum of 5 tags" — which [`max_tags_for`] answers for.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The form the ids of one `ResourceTypeForTagging` are published in, written as the regular
/// expression a refusal of a mis-shaped id states ([`resource_id_format_violation`]), or
/// `None` for a type whose "ID of the resource" is a NAME or an ARN rather than an id of a
/// fixed form (`Document` and `Parameter`, where "use the name of the resource", and
/// `OpsMetadata`, addressed by the tail of its ARN) or an opaque id the service publishes no
/// form for (`Automation`, whose `AutomationExecutionId` is modelled by its length alone, and
/// `ManagedInstance`, whose `InstanceId` admits both an EC2 instance id and a hybrid
/// activation's `mi-` id).
///
/// Each form is the one this crate already reads that type's ids by, so the sentence a refusal
/// states and the check it is taken against cannot drift apart: a maintenance window
/// ([`super::maintenance_windows::is_maintenance_window_id`]) and a patch baseline
/// ([`super::patch_baselines::is_baseline_id`]) carry a prefix and a fixed count of lowercase
/// hexadecimal digits, an OpsItem ([`super::ops_items::is_ops_item_id`]) likewise, and an
/// association ([`super::associations::is_association_id`]) and a cloud connector
/// ([`super::cloud_connectors::is_cloud_connector_id`]) the hyphenated hexadecimal groups
/// their modelled patterns declare.
#[must_use]
pub fn resource_id_format(resource_type: &str) -> Option<&'static str> {
    match resource_type {
        TAGGING_TYPE_ASSOCIATION => Some(
            "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
        ),
        TAGGING_TYPE_CLOUD_CONNECTOR => {
            Some("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        }
        TAGGING_TYPE_MAINTENANCE_WINDOW => Some("mw-[0-9a-f]{17}"),
        TAGGING_TYPE_OPS_ITEM => Some("oi-[0-9a-f]{12}"),
        TAGGING_TYPE_PATCH_BASELINE => Some("pb-[0-9a-f]{17}"),
        _ => None,
    }
}

/// Whether `resource_id` has the form [`resource_id_format`] states for `resource_type`.
fn resource_id_has_its_form(resource_type: &str, resource_id: &str) -> bool {
    match resource_type {
        TAGGING_TYPE_ASSOCIATION => super::associations::is_association_id(resource_id),
        TAGGING_TYPE_CLOUD_CONNECTOR => {
            super::cloud_connectors::is_cloud_connector_id(resource_id)
        }
        TAGGING_TYPE_MAINTENANCE_WINDOW => {
            super::maintenance_windows::is_maintenance_window_id(resource_id)
        }
        TAGGING_TYPE_OPS_ITEM => super::ops_items::is_ops_item_id(resource_id),
        TAGGING_TYPE_PATCH_BASELINE => super::patch_baselines::is_baseline_id(resource_id),
        // A type whose ids have no published form constrains nothing here.
        _ => true,
    }
}

/// Why `(resource_type, resource_id)` names no resource that type could ever have, or `None`
/// when the id has the form that type's ids are published in.
///
/// A tag request addresses a resource by `(ResourceType, ResourceId)`, so the id is read
/// against the form the DECLARED type's ids carry — and that is a check on the REQUEST, taken
/// before anything is looked up: an id of another type's form names no resource of this type
/// whether or not this Region holds one under it, which is why the two refusals are different.
/// A mis-shaped id is the format refusal worded below; a well-formed id the Region holds
/// nothing under is the separate `InvalidResourceId`, which models no message at all. The one
/// reading of that rule for every operation of the tag plane, so a write and a read of one
/// resource cannot disagree about which ids its type admits.
#[must_use]
pub fn resource_id_format_violation(resource_type: &str, resource_id: &str) -> Option<String> {
    let format = resource_id_format(resource_type)?;
    (!resource_id_has_its_form(resource_type, resource_id))
        .then(|| resource_id_format_message(format, resource_type))
}

/// How that refusal is worded: the form the type's ids have, then the type the request
/// declared — fixed text around the two, so a caller is told which id form the type it named
/// requires.
#[must_use]
pub fn resource_id_format_message(format: &str, resource_type: &str) -> String {
    format!("resourceId should be in format {format} for resource type {resource_type}")
}

/// How many tags the resources of `resource_type` may carry.
///
/// "Most resources can have a maximum of 50 tags… Automations can have a maximum of 5
/// tags" (`AddTagsToResource`'s own doc), so the maximum is the resource type's rather
/// than one number for every type — and it is the SAME maximum `StartAutomationExecution`
/// holds the tags it carries to (`super::automation::MAX_TAGS_PER_AUTOMATION`), so the tag
/// plane and the start cannot disagree about how many tags an automation holds.
pub fn max_tags_for(resource_type: &str) -> usize {
    match resource_type {
        TAGGING_TYPE_AUTOMATION => super::automation::MAX_TAGS_PER_AUTOMATION,
        _ => MAX_TAGS_PER_RESOURCE,
    }
}

/// A tag set as the modelled `TagList` reports it, in key order — the order the store
/// holds it in, so two reads of one resource report its tags alike.
pub fn to_tag_list(held: &BTreeMap<String, String>) -> Vec<Tag> {
    held.iter()
        .map(|(key, value)| Tag {
            key: key.clone(),
            value: value.clone(),
        })
        .collect()
}

/// Merge `adding` into `held` — "Adds or overwrites one or more tags", so a key already
/// held takes the submitted value and the set grows only by the keys it did not hold.
///
/// Answers `Err` with the number of tags the resource WOULD then carry when that is more
/// than `maximum` ([`max_tags_for`]), leaving `held` untouched: the merge is all or
/// nothing, so a refused request writes no part of its tags.
pub fn merge_tags(
    held: &mut BTreeMap<String, String>,
    adding: &[Tag],
    maximum: usize,
) -> Result<(), usize> {
    let mut merged = held.clone();
    for tag in adding {
        merged.insert(tag.key.clone(), tag.value.clone());
    }
    if merged.len() > maximum {
        return Err(merged.len());
    }
    *held = merged;
    Ok(())
}

/// Drop every key in `keys` from `held`, which is what a remove does to the resource's
/// tag set. A key the resource does not carry leaves the set as it was: the request
/// asks for those keys to be absent, and they are.
pub fn remove_tags(held: &mut BTreeMap<String, String>, keys: &[String]) {
    for key in keys {
        held.remove(key);
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// The tag set the resource `(resource_type, resource_id)` names in `region`
    /// carries, or `None` when the Region holds no such resource — which is what the tag
    /// operations refuse as an invalid resource id.
    ///
    /// A resource type this store holds no records for resolves to nothing for the same
    /// reason: nothing has created a resource of that type, so no id names one.
    ///
    /// Documents resolve through the records a caller CREATED here. The documents
    /// Systems Manager publishes are owned by Amazon rather than by the account reading
    /// them (`super::PUBLISHED_DOCUMENT_OWNER`), so the account carries no tag set on
    /// one and a published name addresses no resource of its own to tag.
    pub fn resource_tags(
        &self,
        region: &str,
        resource_type: &str,
        resource_id: &str,
    ) -> Option<&BTreeMap<String, String>> {
        match resource_type {
            TAGGING_TYPE_PARAMETER => self.parameter(region, resource_id).map(|held| &held.tags),
            TAGGING_TYPE_DOCUMENT => self.document(region, resource_id).map(|held| &held.tags),
            TAGGING_TYPE_MANAGED_INSTANCE => self
                .managed_node(region, resource_id)
                .map(|held| &held.tags),
            TAGGING_TYPE_OPS_ITEM => self.ops_item(region, resource_id).map(|held| &held.tags),
            TAGGING_TYPE_OPS_METADATA => self
                .ops_metadata_object(region, resource_id)
                .map(|held| &held.tags),
            TAGGING_TYPE_CLOUD_CONNECTOR => self
                .cloud_connector(region, resource_id)
                .map(|held| &held.tags),
            TAGGING_TYPE_AUTOMATION => self
                .automation_execution(region, resource_id)
                .map(|held| &held.tags),
            TAGGING_TYPE_ASSOCIATION => {
                self.association(region, resource_id).map(|held| &held.tags)
            }
            TAGGING_TYPE_MAINTENANCE_WINDOW => self
                .maintenance_window(region, resource_id)
                .map(|held| &held.tags),
            _ => None,
        }
    }

    /// The same tag set, to write it. See [`ComAmazonawsSsmAmazonssmState::resource_tags`]
    /// for how a request addresses a resource.
    pub fn resource_tags_mut(
        &mut self,
        region: &str,
        resource_type: &str,
        resource_id: &str,
    ) -> Option<&mut BTreeMap<String, String>> {
        match resource_type {
            TAGGING_TYPE_PARAMETER => self
                .parameter_mut(region, resource_id)
                .map(|held| &mut held.tags),
            TAGGING_TYPE_DOCUMENT => self
                .document_mut(region, resource_id)
                .map(|held| &mut held.tags),
            TAGGING_TYPE_MANAGED_INSTANCE => self
                .managed_node_mut(region, resource_id)
                .map(|held| &mut held.tags),
            TAGGING_TYPE_OPS_ITEM => self
                .ops_item_mut(region, resource_id)
                .map(|held| &mut held.tags),
            TAGGING_TYPE_OPS_METADATA => self
                .ops_metadata_object_mut(region, resource_id)
                .map(|held| &mut held.tags),
            TAGGING_TYPE_CLOUD_CONNECTOR => self
                .cloud_connector_mut(region, resource_id)
                .map(|held| &mut held.tags),
            TAGGING_TYPE_AUTOMATION => self
                .automation_execution_mut(region, resource_id)
                .map(|held| &mut held.tags),
            TAGGING_TYPE_ASSOCIATION => self
                .association_mut(region, resource_id)
                .map(|held| &mut held.tags),
            TAGGING_TYPE_MAINTENANCE_WINDOW => self
                .maintenance_window_mut(region, resource_id)
                .map(|held| &mut held.tags),
            _ => None,
        }
    }
}
