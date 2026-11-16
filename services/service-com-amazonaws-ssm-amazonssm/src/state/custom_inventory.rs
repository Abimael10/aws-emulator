// SPDX-License-Identifier: CC0-1.0

//! The CUSTOM inventory a caller writes about its managed nodes, and the deletions that
//! remove it again.
//!
//! Systems Manager Inventory is organised by TYPE ([`super::inventory`]). The types in the
//! `AWS:` namespace are the ones Systems Manager publishes schemas for and gathers itself —
//! `AWS:InstanceInformation` is the projection of the node record, not a second copy of it —
//! while the `Custom:` namespace is the caller's: "Bulk update custom inventory items on one
//! or more managed nodes. The request adds an inventory item, if it doesn't already exist,
//! or updates an inventory item, if it does exist" (`PutInventory`). So this store holds
//! exactly what a put wrote, keyed by `(region, node id, type name)`: one item per type per
//! node, which is what makes a second put of the same type an UPDATE of the item rather than
//! a second one.
//!
//! A custom inventory TYPE exists in a Region for as long as some node there holds an item
//! of it — a schema is not registered separately, it is what the items of that type are
//! reported under — so [`custom_types_in_region`] reads the types off the items rather than
//! from a second store. That is what `DeleteInventory` addresses: "Delete a custom inventory
//! type or the data associated with a custom Inventory type. Deleting a custom inventory
//! type is also referred to as deleting a custom inventory schema", so a type name outside
//! the `Custom:` namespace, and one no node holds an item of, name nothing a delete can
//! remove ([`InvalidTypeNameException`](crate::ops::types::InvalidTypeNameException)).
//!
//! A delete is RECORDED ([`InventoryDeletionRecord`]), keyed by the `DeletionId` it minted,
//! for two reasons: the id a delete hands back has to resolve to the deletion it names, and
//! `DeleteInventory`'s `ClientToken` is an `@idempotencyToken`, so a repeat of the same
//! request is the same deletion rather than a second one.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{
    InventoryDeletionStatusItem, InventoryDeletionSummary, InventoryDeletionSummaryItem,
    InventoryItemAttribute, InventoryItemSchema,
};

use super::inventory::CUSTOM_TYPE_PREFIX;

/// How many custom inventory types one Region may hold items of.
///
/// The EMULATOR's own ceiling rather than a quota Systems Manager publishes: a put is the
/// only thing that admits a type, callers may write as many as they like, and the store is
/// held in memory for as long as the process runs. A put that would admit one past it is
/// refused through the modelled `CustomSchemaCountLimitExceededException` and writes
/// nothing.
pub const MAX_CUSTOM_INVENTORY_TYPES: usize = 20;

/// How many inventory items this store holds at once, across every Region and node — the
/// emulator's own bound on the one map a put grows, for the same reason
/// [`MAX_CUSTOM_INVENTORY_TYPES`] exists.
pub const MAX_CUSTOM_INVENTORY_ITEMS: usize = 1_000;

/// What a put refused by [`MAX_CUSTOM_INVENTORY_TYPES`] reports: the emulator's own ceiling,
/// named as the emulator's, with the figure it stands at.
#[must_use]
pub fn custom_schema_ceiling_message() -> String {
    format!(
        "This emulator holds at most {MAX_CUSTOM_INVENTORY_TYPES} custom inventory types per \
         Region"
    )
}

/// What a put refused by [`MAX_CUSTOM_INVENTORY_ITEMS`] reports.
#[must_use]
pub fn custom_item_ceiling_message() -> String {
    format!(
        "This emulator holds at most {MAX_CUSTOM_INVENTORY_ITEMS} inventory items across every \
         Region"
    )
}

/// How this service refuses a type name a WRITE of custom inventory may not carry: the name
/// the caller submitted and the namespace a custom type is written under.
///
/// `PutInventory` writes "custom inventory items", so the `AWS:` namespace — whose schemas
/// Systems Manager publishes and whose items it gathers itself — is not a caller's to write
/// in, whichever of those types a put names.
#[must_use]
pub fn uncustomizable_type_name_message(type_name: &str) -> String {
    format!(
        "The parameter type name isn't valid: {type_name}. Custom inventory type names start \
         with {CUSTOM_TYPE_PREFIX}."
    )
}

/// How this service refuses a type name a DELETE cannot address because it is not a custom
/// type: "Delete a custom inventory type or the data associated with a custom Inventory
/// type" (`DeleteInventory`), so a name outside the `Custom:` namespace names one of the
/// types Systems Manager itself predefines ([`super::inventory::DEFAULT_INVENTORY_TYPES`]),
/// which is not the account's to delete.
///
/// RECORDED BEHAVIOUR (corpus `simparity-behavioral-aws-v1`, unit
/// `ssm.delete-inventory-schema-delete-builtin-type-rejected`): a `DeleteInventory` naming a
/// type of the `AWS:` namespace is answered 400 `InvalidTypeNameException` with
/// `Deleting predefined type names is not allowed.` — fixed text, with nothing of the
/// request in it, so the name the caller submitted does not appear.
pub const PREDEFINED_TYPE_NOT_DELETABLE: &str = "Deleting predefined type names is not allowed.";

/// How this service refuses a custom type name the Region holds no inventory of: the name
/// names no schema here, so there is nothing to delete or disable under it.
#[must_use]
pub fn unknown_custom_type_message(type_name: &str) -> String {
    format!("The parameter type name isn't valid: {type_name}. No inventory of that type exists.")
}

/// How this service refuses a type name that names no inventory type at all: it is neither
/// one of the types Systems Manager publishes ([`super::inventory::DEFAULT_INVENTORY_TYPES`])
/// nor a name of the caller's own namespace, so no schema could be held under it.
#[must_use]
pub fn unknown_type_name_message(type_name: &str) -> String {
    format!(
        "The parameter type name isn't valid: {type_name}. An inventory type name is \
         {CUSTOM_TYPE_PREFIX}<name> or one of the type names Systems Manager publishes."
    )
}

/// Whether `type_name` is a name of the caller's own inventory namespace: "Custom inventory
/// type names will start with Custom" (`InventoryItemSchema$TypeName`).
///
/// The modelled `InventoryItemTypeName` pattern (`^(AWS|Custom):.*$`) is what admits a value
/// here at all, so this tells the two namespaces apart rather than checking the form.
#[must_use]
pub fn is_custom_type(type_name: &str) -> bool {
    type_name.starts_with(CUSTOM_TYPE_PREFIX)
}

/// One custom inventory item, as the put persisted it.
#[derive(Debug, Clone)]
pub struct CustomInventoryItemRecord {
    /// The Region whose endpoint the put was sent to.
    pub region: String,
    /// The node the item is about, which is the node the put named.
    pub node_id: String,
    /// "The name of the inventory type" this item belongs to.
    pub type_name: String,
    /// "The schema version for the inventory item", as the put supplied it.
    pub schema_version: String,
    /// "The time the inventory information was collected", as the put supplied it: the
    /// instant the GUEST reported, not the instant the call arrived, so it is held as given.
    pub capture_time: String,
    /// "MD5 hash of the inventory item type contents", as the put supplied it; `None` for a
    /// put that carried none.
    pub content_hash: Option<String>,
    /// "The inventory data of the inventory type" — the entries the put supplied, held
    /// exactly as they came so a read reports what was written.
    pub content: Vec<HashMap<String, String>>,
    /// When the put that persisted this item ran.
    pub written_at: SystemTime,
}

/// How this service reports a `DeletionId` this endpoint recorded no deletion under, through
/// `InvalidDeletionIdException`: the id was returned by no `DeleteInventory` of this
/// endpoint. Fixed text for this error class, with nothing of the request in it.
pub const UNKNOWN_DELETION_ID: &str =
    "The ID specified for the delete operation doesn't exist or isn't valid. Verify the ID and \
     try again.";

/// The status a recorded deletion reports, from the modelled `InventoryDeletionStatus`
/// (`InProgress | Complete`): a deletion this store performs is done when the call that
/// performed it returns — the items are gone before the response is written — so every
/// recorded deletion is complete, and so is a dry run, which was never going to remove
/// anything.
pub const DELETION_STATUS_COMPLETE: &str = "Complete";

/// One recorded inventory deletion, as `DeleteInventory` minted it.
#[derive(Debug, Clone)]
pub struct InventoryDeletionRecord {
    /// "Every `DeleteInventory` operation is assigned a unique ID" — the id this deletion is
    /// reached by, minted by the delete.
    pub deletion_id: String,
    /// The Region whose endpoint the delete was sent to.
    pub region: String,
    /// The inventory type the delete addressed.
    pub type_name: String,
    /// The `ClientToken` the delete carried, which is what makes a repeat of the same
    /// request the same deletion ([`super::ComAmazonawsSsmAmazonssmState::inventory_deletion_of_token`]).
    pub client_token: Option<String>,
    /// How many items the delete removed, and how many of them are left — a deletion this
    /// store performs is complete when it returns, so nothing remains.
    pub total_count: i32,
    /// The schema versions the removed items carried, with how many carried each.
    pub counts_by_version: BTreeMap<String, i32>,
    /// Whether the delete was a `DryRun`, which "views a summary of the deletion request
    /// without deleting any data".
    pub dry_run: bool,
    /// Whether the delete removed the SCHEMA as well as the data
    /// (`InventorySchemaDeleteOption`).
    pub schema_delete_option: Option<String>,
    /// When the delete ran.
    pub deleted_at: SystemTime,
}

impl InventoryDeletionRecord {
    /// This deletion as the modelled `InventoryDeletionStatusItem` reports it: what it
    /// addressed, when it ran, the status it reached and the summary of what it found.
    ///
    /// `LastStatusUpdateTime` is the same instant as the start: the deletion was complete when
    /// the call that performed it returned, so nothing updated its status afterwards. No
    /// message is reported beside the status, because there is nothing to say about a
    /// deletion that completed.
    pub fn to_status_item(&self) -> InventoryDeletionStatusItem {
        let at = ::aws_smithy_types::DateTime::from(self.deleted_at);
        InventoryDeletionStatusItem {
            deletion_id: Some(self.deletion_id.clone()),
            type_name: Some(self.type_name.clone()),
            deletion_start_time: Some(at),
            last_status: Some(DELETION_STATUS_COMPLETE.to_string()),
            last_status_message: None,
            deletion_summary: Some(self.to_summary()),
            last_status_update_time: Some(at),
        }
    }

    /// This deletion as the modelled `InventoryDeletionSummary` reports it: how many items it
    /// addressed, how many are left, and one summary item per schema version it found.
    ///
    /// A DRY RUN deletes nothing, so every item it counted remains; a real delete leaves
    /// none, because the removal is complete when the call returns.
    pub fn to_summary(&self) -> InventoryDeletionSummary {
        let remaining = |count: i32| if self.dry_run { count } else { 0 };
        InventoryDeletionSummary {
            total_count: self.total_count,
            remaining_count: remaining(self.total_count),
            summary_items: Some(
                self.counts_by_version
                    .iter()
                    .map(|(version, count)| InventoryDeletionSummaryItem {
                        version: Some(version.clone()),
                        count: *count,
                        remaining_count: remaining(*count),
                    })
                    .collect(),
            ),
        }
    }
}

/// The data type an inventory attribute's values carry, as the modelled
/// `InventoryAttributeDataType` spells it: the type of everything a node reported under that
/// attribute.
pub const ATTRIBUTE_DATA_TYPE_STRING: &str = "string";

/// See [`ATTRIBUTE_DATA_TYPE_STRING`]: the other value of that enum, which an attribute
/// carries when every value reported under it is a number.
pub const ATTRIBUTE_DATA_TYPE_NUMBER: &str = "number";

/// The schema of the custom type `type_name`, read off the items `items` holds of it.
///
/// A custom type's schema is not a record of its own — nothing registers one, a put is what
/// admits the type (see the module docs) — so the schema IS what the items say: its
/// attributes are the names their content carries, in ascending name order, and each
/// attribute's data type is `number` only where every value reported under it is one
/// ([`ATTRIBUTE_DATA_TYPE_NUMBER`]). The version is the one the LAST put wrote, which is the
/// schema version the type is currently reported under.
///
/// `None` for a type whose items name no attribute at all: `InventoryItemSchema$Attributes`
/// is `@required` with at least one attribute, so there is no schema to report for one.
#[must_use]
pub fn custom_type_schema<'a>(
    type_name: &str,
    items: impl IntoIterator<Item = &'a CustomInventoryItemRecord>,
) -> Option<InventoryItemSchema> {
    let mut numeric: BTreeMap<String, bool> = BTreeMap::new();
    let mut version: Option<(SystemTime, String)> = None;
    for item in items {
        for entry in &item.content {
            for (name, value) in entry {
                let numbers = value.parse::<f64>().is_ok();
                numeric
                    .entry(name.clone())
                    .and_modify(|held| *held = *held && numbers)
                    .or_insert(numbers);
            }
        }
        if version
            .as_ref()
            .is_none_or(|(written, _)| *written <= item.written_at)
        {
            version = Some((item.written_at, item.schema_version.clone()));
        }
    }
    if numeric.is_empty() {
        return None;
    }
    Some(InventoryItemSchema {
        type_name: type_name.to_string(),
        version: version.map(|(_, version)| version),
        attributes: numeric
            .into_iter()
            .map(|(name, numbers)| InventoryItemAttribute {
                name,
                data_type: if numbers {
                    ATTRIBUTE_DATA_TYPE_NUMBER.to_string()
                } else {
                    ATTRIBUTE_DATA_TYPE_STRING.to_string()
                },
            })
            .collect(),
        // "The alias name of the inventory type. The alias name is used for display
        // purposes": a custom type carries the name its put wrote and no alias beside it, so
        // none is invented.
        display_name: None,
    })
}

impl super::ComAmazonawsSsmAmazonssmState {
    /// The custom inventory items `region` holds of `type_name`, in ascending node-id order.
    pub fn custom_inventory_of_type<'a>(
        &'a self,
        region: &'a str,
        type_name: &'a str,
    ) -> impl Iterator<Item = &'a CustomInventoryItemRecord> + 'a {
        self.custom_inventory
            .iter()
            .filter(move |((held_region, _, held_type), _)| {
                held_region == region && held_type == type_name
            })
            .map(|(_, record)| record)
    }

    /// The custom inventory items `region` holds about the node `node_id`, in ascending
    /// type-name order.
    pub fn custom_inventory_of_node<'a>(
        &'a self,
        region: &'a str,
        node_id: &'a str,
    ) -> impl Iterator<Item = &'a CustomInventoryItemRecord> + 'a {
        self.custom_inventory
            .iter()
            .filter(move |((held_region, held_node, _), _)| {
                held_region == region && held_node == node_id
            })
            .map(|(_, record)| record)
    }

    /// Every custom inventory type `region` holds an item of: a schema exists for as long as
    /// some node holds an item under it (see the module docs).
    pub fn custom_inventory_types_in_region<'a>(
        &'a self,
        region: &'a str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        self.custom_inventory
            .keys()
            .filter(move |(held_region, _, _)| held_region == region)
            .map(|(_, _, type_name)| type_name.as_str())
    }

    /// Store `record` as the one item of its type for its node, replacing the item that node
    /// held of that type — "adds an inventory item, if it doesn't already exist, or updates
    /// an inventory item, if it does exist" (`PutInventory`).
    pub fn insert_custom_inventory(&mut self, record: CustomInventoryItemRecord) {
        let key = (
            record.region.clone(),
            record.node_id.clone(),
            record.type_name.clone(),
        );
        self.custom_inventory.insert(key, record);
    }

    /// Drop every item of `type_name` that `region` holds, answering how many went.
    pub fn remove_custom_inventory_of_type(&mut self, region: &str, type_name: &str) -> usize {
        let before = self.custom_inventory.len();
        self.custom_inventory
            .retain(|(held_region, _, held_type), _| {
                held_region != region || held_type != type_name
            });
        before - self.custom_inventory.len()
    }

    /// Drop every item `region` holds about the node `node_id`, answering how many went.
    ///
    /// The inventory of a node is inventory ABOUT that node, so a node that is no longer
    /// registered holds none: `DeregisterManagedInstance` takes its items with it rather than
    /// leaving items about a node no read resolves.
    pub fn remove_custom_inventory_of_node(&mut self, region: &str, node_id: &str) -> usize {
        let before = self.custom_inventory.len();
        self.custom_inventory
            .retain(|(held_region, held_node, _), _| {
                held_region != region || held_node != node_id
            });
        before - self.custom_inventory.len()
    }

    /// Record `record` as a deletion of its own Region, keyed by the id it minted.
    pub fn insert_inventory_deletion(&mut self, record: InventoryDeletionRecord) {
        let key = (record.region.clone(), record.deletion_id.clone());
        self.inventory_deletions.insert(key, record);
    }

    /// The deletion `region` recorded for `client_token`, or `None` when that Region has
    /// recorded none — which is what makes a delete carrying a token it has already served
    /// the SAME deletion rather than a second one (`DeleteInventoryRequest$ClientToken` is an
    /// `@idempotencyToken`).
    pub fn inventory_deletion_of_token(
        &self,
        region: &str,
        client_token: &str,
    ) -> Option<&InventoryDeletionRecord> {
        self.inventory_deletions
            .iter()
            .filter(|((held_region, _), _)| held_region == region)
            .map(|(_, record)| record)
            .find(|record| record.client_token.as_deref() == Some(client_token))
    }

    /// Every deletion `region` has recorded, in ascending deletion-id order — the store's own
    /// order, which is what lets a page resume from the id it stopped at rather than from an
    /// offset.
    pub fn inventory_deletions_in_region<'a>(
        &'a self,
        region: &'a str,
    ) -> impl Iterator<Item = &'a InventoryDeletionRecord> + 'a {
        self.inventory_deletions
            .iter()
            .filter(move |((held_region, _), _)| held_region == region)
            .map(|(_, record)| record)
    }

    /// Whether this store can record another deletion: the same bound the items carry
    /// ([`MAX_CUSTOM_INVENTORY_ITEMS`]), since a delete is admitted by a call rather than by
    /// a resource and the one map would otherwise grow for as long as deletes keep arriving.
    pub fn can_record_another_inventory_deletion(&self) -> bool {
        self.inventory_deletions.len() < MAX_CUSTOM_INVENTORY_ITEMS
    }
}
