// SPDX-License-Identifier: CC0-1.0

//! The inventory this service holds about the managed nodes of a Region, and how an
//! inventory query reads it.
//!
//! Systems Manager Inventory is organised by TYPE: every inventory item a node reports
//! belongs to one type (`AWS:InstanceInformation`, `AWS:Application`, `AWS:Network`, a
//! `Custom:` type a caller wrote — `InventoryItemSchema$TypeName`), and every attribute of
//! an item is named by its type: `AWS:InstanceInformation.PlatformType`. That dotted form
//! is what an inventory filter key and an aggregator expression carry
//! ([`split_qualified_attribute`]), which is why a key naming no type, or naming no
//! attribute of a type whose schema this service knows, cannot be served at all.
//!
//! The one type this store holds data of is `AWS:InstanceInformation`: "Query inventory
//! information. This includes managed node status, such as `Stopped` or `Terminated`"
//! (`GetInventory`) — the item every managed node has by being registered, projected from
//! the node record the node describes report
//! ([`super::ManagedNodeRecord`], [`instance_information_content`]) rather than stored a
//! second time. Nothing here invents a node or an item: an account that has registered no
//! node holds no inventory, and an attribute the node record holds no value for is
//! reported by nothing and selected by no filter — the same rule
//! [`super::ManagedNodeRecord::filter_values`] follows for the node describes. The other
//! types Systems Manager publishes ([`super::inventory_catalog`]) are types this service
//! knows the schema of and holds no item of, because the only item derived here is the node's
//! own and the only items a caller writes are the `Custom:` ones
//! ([`super::custom_inventory`], written by `PutInventory` and removed by `DeleteInventory`):
//! a query for one of those types is answered with the nothing that has been collected for
//! it, never refused.
//!
//! AGGREGATION is narrower than filtering: only some attributes of a type can be counted
//! over. "Returns inventory schemas that support aggregation. For example, this call
//! returns the `AWS:InstanceInformation` type, because it supports aggregation based on the
//! `PlatformName`, `PlatformType`, and `PlatformVersion` attributes"
//! (`GetInventorySchemaRequest$Aggregator`) — so those three are what an aggregator
//! expression over this type may name ([`INSTANCE_INFORMATION_AGGREGATABLE_ATTRIBUTES`]),
//! and an expression naming another attribute of it is refused rather than counted.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::ops::types::InventoryItemSchema;

use super::ManagedNodeRecord;

/// The inventory type every managed node has by being registered: the node's own
/// information, which is the default type an inventory query reports ("Valid value:
/// `AWS:InstanceInformation`. Default Value: `AWS:InstanceInformation`" —
/// `ResultAttribute$TypeName`).
pub const INVENTORY_TYPE_INSTANCE_INFORMATION: &str = "AWS:InstanceInformation";

/// The schema version the ITEMS of that type carry here (`InventoryResultItem$SchemaVersion`,
/// pattern `^([0-9]{1,6})(\.[0-9]{1,6})$`): the first version of that schema, which is the
/// version the content projected from the node record belongs to — the attributes
/// [`instance_information_attribute`] answers are the ones that version carries, and nothing
/// here collects the ones the published revision added.
///
/// It is NOT the version the CATALOG publishes for the type: that is the current schema
/// version, which [`super::inventory_catalog`] holds beside the type's full attribute list.
pub const INSTANCE_INFORMATION_SCHEMA_VERSION: &str = "1.0";

/// The prefix of a type name a caller's own schema carries, as against the Amazon ones:
/// "Custom inventory type names will start with Custom" (`InventoryItemSchema$TypeName`).
pub const CUSTOM_TYPE_PREFIX: &str = "Custom:";

/// The attributes an `AWS:InstanceInformation` item is made of, which are the ones the
/// published schema of that type carries
/// ([`super::inventory_catalog::INSTANCE_INFORMATION_ATTRIBUTES`]) — the catalog is the one
/// statement of what a type's attributes are, so a filter key, an aggregator expression and a
/// reported schema all name the same set.
///
/// Each is one property of the registered node ([`instance_information_attribute`]), for the
/// ones this endpoint holds a property for: the node's identity and kind, the agent it runs,
/// the platform it runs on, its address and name, and the node status `GetInventory`
/// documents this type as carrying. An attribute no node record holds is reported by nothing
/// and selected by no filter, the same answer the node describes give for a property nothing
/// has recorded.
pub const INSTANCE_INFORMATION_ATTRIBUTES: &[&str] =
    super::inventory_catalog::INSTANCE_INFORMATION_ATTRIBUTES;

/// The attribute naming WHICH agent reported the item. The node store records the agent's
/// version but not its implementation ([`super::ManagedNodeRecord`] holds the modelled
/// `InstanceProperty`, which has no member for it), so no node holds a value for this
/// attribute and no filter on it selects one — the same answer the node describes give a
/// filter on a property nothing has recorded.
pub const ATTRIBUTE_AGENT_TYPE: &str = "AgentType";

/// The attributes of [`INVENTORY_TYPE_INSTANCE_INFORMATION`] an aggregator may count over:
/// "this call returns the `AWS:InstanceInformation` type, because it supports aggregation
/// based on the `PlatformName`, `PlatformType`, and `PlatformVersion` attributes"
/// (`GetInventorySchemaRequest$Aggregator`).
pub const INSTANCE_INFORMATION_AGGREGATABLE_ATTRIBUTES: &[&str] =
    &["PlatformName", "PlatformType", "PlatformVersion"];

/// The name of the inventory policy document Systems Manager publishes: the document a
/// client associates with its nodes to have inventory collected from them (AWS Systems
/// Manager User Guide, "Configuring inventory collection", which associates
/// `AWS-GatherSoftwareInventory` with the nodes to inventory).
///
/// It is a `Policy` document ([`super::DOCUMENT_TYPE_POLICY`]) rather than a command: it
/// tells the agent on each target node WHAT to collect, which is why it is applied by
/// creating an association rather than by sending a command.
pub const GATHER_SOFTWARE_INVENTORY_DOCUMENT: &str = "AWS-GatherSoftwareInventory";

/// The body of [`GATHER_SOFTWARE_INVENTORY_DOCUMENT`]: the one `aws:softwareInventory`
/// step, and a parameter per inventory type the agent can be asked for.
///
/// Every parameter declares a DEFAULT, which is what lets an association be created for
/// this document without supplying any: "all of these parameters are optional"
/// (same user guide section) — the types collected by default are enabled and the ones a
/// client has to name the paths or the keys of are off.
pub const GATHER_SOFTWARE_INVENTORY: &str = r#"{
  "schemaVersion": "2.0",
  "description": "Software Inventory Policy Document.",
  "parameters": {
    "applications": {
      "type": "String",
      "description": "(Optional) Collect data for installed applications.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    },
    "awsComponents": {
      "type": "String",
      "description": "(Optional) Collect data for AWS Components like amazon-ssm-agent.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    },
    "networkConfig": {
      "type": "String",
      "description": "(Optional) Collect data for Network configurations.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    },
    "instanceDetailedInformation": {
      "type": "String",
      "description": "(Optional) Collect additional information about the instance, including the CPU model, speed, and the number of cores, to name a few.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    },
    "services": {
      "type": "String",
      "description": "(Optional, Windows OS only) Collect data for service configurations.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    },
    "windowsRoles": {
      "type": "String",
      "description": "(Optional, Windows OS only) Collect data for Microsoft Windows role configurations.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    },
    "windowsUpdates": {
      "type": "String",
      "description": "(Optional, Windows OS only) Collect data for all Windows Updates.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    },
    "files": {
      "type": "String",
      "description": "(Optional, requires SSMAgent version 2.2.64.0 and above) Collect data for files, provided as a list of JSON objects.",
      "default": "",
      "displayType": "textarea"
    },
    "windowsRegistry": {
      "type": "String",
      "description": "(Optional, requires SSMAgent version 2.2.64.0 and above) Collect data for Microsoft Windows registry keys and values, provided as a list of JSON objects.",
      "default": "",
      "displayType": "textarea"
    },
    "customInventory": {
      "type": "String",
      "description": "(Optional) Collect data for custom inventory.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    },
    "billingInfo": {
      "type": "String",
      "description": "(Optional) Collect billing info for license includes AMIs.",
      "default": "Enabled",
      "allowedValues": ["Enabled", "Disabled"]
    }
  },
  "mainSteps": [
    {
      "action": "aws:softwareInventory",
      "name": "collectSoftwareInventoryItems",
      "inputs": {
        "applications": "{{ applications }}",
        "awsComponents": "{{ awsComponents }}",
        "networkConfig": "{{ networkConfig }}",
        "files": "{{ files }}",
        "services": "{{ services }}",
        "windowsRoles": "{{ windowsRoles }}",
        "windowsRegistry": "{{ windowsRegistry }}",
        "windowsUpdates": "{{ windowsUpdates }}",
        "instanceDetailedInformation": "{{ instanceDetailedInformation }}",
        "customInventory": "{{ customInventory }}",
        "billingInfo": "{{ billingInfo }}"
      }
    }
  ]
}"#;

/// The inventory query operator that asks whether the attribute is reported at all, rather
/// than comparing its value (the modelled `InventoryQueryOperatorType` value `Exists`, the
/// one operator the node describes' `InstancePropertyFilterOperator` does not carry).
///
/// It is the criterion an AGGREGATOR's groups may be defined by, and not a way of SELECTING
/// the nodes a query reports: "The `Exists` filter must be used with aggregators"
/// (`InventoryFilter$Type`), so a query's own filters are refused for naming it
/// ([`EXISTS_ONLY_IN_AGGREGATOR_FILTERS`]).
pub const OPERATOR_EXISTS: &str = "Exists";

/// How this service states the [`OPERATOR_EXISTS`] operator in a query's own filters, where
/// it belongs to an aggregator's group criteria alone.
pub const EXISTS_ONLY_IN_AGGREGATOR_FILTERS: &str =
    "Exists operator can only be used in Aggregator filters.";

/// The key an aggregated entry carries its count under, beside the attribute value it
/// counted: an aggregator "returns counts of inventory types based on one or more
/// expressions … you can see a count of how many Windows and Linux managed nodes exist in
/// your inventoried fleet" (`GetInventoryRequest$Aggregators`).
pub const AGGREGATION_COUNT_KEY: &str = "Count";

/// The prefix the count of the resources that do NOT match a group's criteria is reported
/// under: "Groups return a count of resources that match and don't match the specified
/// criteria" (`InventoryAggregator$Groups`), so a group of name `N` is reported as the
/// count under `N` beside the count under `NotN`.
pub const GROUP_COMPLEMENT_PREFIX: &str = "Not";

/// The `Id` of an entity that reports counts rather than one node: an aggregated count is
/// about the fleet, so it belongs to no node — "ID of the inventory result entity. For
/// example, for managed node inventory the result will be the managed node ID"
/// (`InventoryResultEntity$Id`), and a count is the inventory of no single node.
pub const AGGREGATED_ENTITY_ID: &str = "";

/// How this service states a filter key that names no attribute of an inventory type.
/// `InvalidFilter`'s own documentation: "The filter name isn't valid. Verify that you
/// entered the correct name and try again."
pub const INVALID_FILTER: &str =
    "The filter name isn't valid. Verify that you entered the correct name and try again.";

/// How this service states an aggregator naming no one aggregation at all — neither an
/// expression nor groups, or both at once. `InvalidAggregatorException`'s own documentation:
/// "The specified aggregator isn't valid for the group type. Verify that the aggregator you
/// provided is supported."
///
/// An expression that is not an expression, or that names an attribute its type supports no
/// aggregation on, is a different refusal of the same class, stated by
/// [`invalid_expression_message`] and [`not_aggregatable_message`]: those name the value or
/// the attribute and type, which is what tells a caller WHICH of up to ten aggregators was
/// rejected.
pub const INVALID_AGGREGATOR: &str = "The specified aggregator isn't valid for the group type. \
                                      Verify that the aggregator you provided is supported.";

/// How many counted entries one aggregated read may report, and so how many a query may
/// ask it to count: the most an inventory item's content may carry
/// (`InventoryResultItem$Content` targets `InventoryItemEntryList`, `len 0..10000`), since
/// a count is reported as an entry of that content and an item cannot carry more.
///
/// Aggregating by groups MULTIPLIES: the entries of a nested aggregator are asked for once
/// per group of the aggregator that nests it, so a request that grows by one line asks for
/// a multiple of the entries, which is why how many a request asks for is counted before a
/// node is read and a query asking for more than an item may carry is refused.
pub const MAX_AGGREGATED_ENTRIES: usize = 10_000;

/// The lower bound the modelled `MaxResults` of an inventory query carries
/// (`GetInventoryRequest$MaxResults`, range 1..50).
pub const MIN_INVENTORY_PAGE_SIZE: i32 = 1;
/// The upper bound of the same member, and the page size a request naming no `MaxResults`
/// is served up to (see [`super::page_size`]).
pub const MAX_INVENTORY_PAGE_SIZE: i32 = 50;

/// The page size an inventory query serves, or the refusal message for a `MaxResults`
/// outside the modelled range — which the boundary validator already refuses, so this is a
/// backstop rather than the first line of defence.
pub fn inventory_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_INVENTORY_PAGE_SIZE,
        MAX_INVENTORY_PAGE_SIZE,
    )
}

/// The page a read of ONE node's inventory entries serves (`ListInventoryEntriesRequest`'s
/// `MaxResults`, range 1..50 — the same bounds the cross-node query carries, so the one
/// reading answers both).
pub fn inventory_entry_page_size(max_results: Option<i32>) -> Result<usize, String> {
    inventory_page_size(max_results)
}

/// The inventory type and the attribute of it a dotted name carries
/// (`AWS:InstanceInformation.PlatformType` → `("AWS:InstanceInformation",
/// "PlatformType")`), or `None` when the name is not that form.
///
/// A type name is `AWS:` or `Custom:` followed by the type itself
/// (`InventoryItemTypeName`, pattern `^(AWS|Custom):.*$`) and the attribute is what
/// follows the first `.` after it, so neither part may be empty.
pub fn split_qualified_attribute(name: &str) -> Option<(&str, &str)> {
    let (prefix, rest) = name.split_once(':')?;
    if !matches!(prefix, "AWS" | "Custom") {
        return None;
    }
    let (type_tail, attribute) = rest.split_once('.')?;
    (!type_tail.is_empty() && !attribute.is_empty())
        .then(|| (&name[..prefix.len() + 1 + type_tail.len()], attribute))
}

/// Whether `type_name` names an inventory type this service knows: one of the types Systems
/// Manager publishes, spelled exactly ([`super::inventory_catalog::is_published_type`]), or a
/// name of the caller's own namespace satisfying the custom-name pattern
/// ([`super::inventory_catalog::is_valid_custom_type_name`]).
///
/// The modelled `^(AWS|Custom):.*$` pattern admits the FORM of a name; this is the service's
/// own rule about what such a name may be, and a value outside it names no inventory type at
/// all ([`super::inventory_catalog::invalid_type_name_message`]).
pub fn is_known_inventory_type(type_name: &str) -> bool {
    super::inventory_catalog::is_published_type(type_name)
        || super::inventory_catalog::is_valid_custom_type_name(type_name)
}

/// Why an inventory filter key cannot be served: the key's FORM, the type it names and the
/// attribute it names are three different things to get wrong, and the type-name fault is the
/// modelled type-name refusal rather than a filter one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterKeyRejection {
    /// The key is not the `<TypeName>.<AttributeName>` form at all, so it names no attribute
    /// of any type; stated by [`invalid_filter_key_message`].
    NotAKey(String),
    /// The key's type-name part names no inventory type ([`is_known_inventory_type`]), which
    /// is a type-name fault; stated by
    /// [`super::inventory_catalog::invalid_filter_key_type_name_message`].
    UnknownTypeName {
        /// The type name the key carries.
        type_name: String,
        /// The whole key it was read from.
        key: String,
    },
    /// The key names an attribute outside the schema of a type whose schema this service
    /// holds; stated by [`undefined_attribute_message`].
    UnknownAttribute(String),
}

/// How this service states a filter key that is not the `<TypeName>.<AttributeName>` form:
/// the key the request submitted, the form a key has to take, and an example of one.
///
/// Fixed text apart from the key, which is echoed from the request.
#[must_use]
pub fn invalid_filter_key_message(key: &str) -> String {
    format!(
        "Invalid filter key {key}. The key format must be TypeName.AttributeName, such as \
         AWS:Application.Name."
    )
}

/// How this service states a filter key naming an attribute the inventory type it names does
/// not define: the attribute, quoted, and that the type defines no such attribute.
///
/// Fixed text apart from the quoted attribute, which is echoed from the key.
#[must_use]
pub fn undefined_attribute_message(attribute: &str) -> String {
    format!("Attribute name '{attribute}' is not defined for the given type")
}

/// Why an inventory filter key cannot be served, or `None` when it can.
///
/// A key has to name an attribute of an inventory type: the inventory type up to the first
/// `.` and the attribute of it after that, so a value missing either half is not a key at all
/// and is quoted back as one. A key that IS that form has to name a type this service knows;
/// for the one type whose schema this service holds, the attribute has to be one of that
/// schema's attributes; for a type it knows and holds no schema of, any attribute name is a
/// name that type may publish, so the filter is served — against the nothing that type has
/// collected — rather than refused.
pub fn filter_key_rejection(key: &str) -> Option<FilterKeyRejection> {
    let (type_name, attribute) = key.split_once('.').unwrap_or((key, ""));
    if type_name.is_empty() || attribute.is_empty() {
        return Some(FilterKeyRejection::NotAKey(key.to_string()));
    }
    if !is_known_inventory_type(type_name) {
        return Some(FilterKeyRejection::UnknownTypeName {
            type_name: type_name.to_string(),
            key: key.to_string(),
        });
    }
    let unknown_attribute = type_name == INVENTORY_TYPE_INSTANCE_INFORMATION
        && !INSTANCE_INFORMATION_ATTRIBUTES.contains(&attribute);
    unknown_attribute.then(|| FilterKeyRejection::UnknownAttribute(attribute.to_string()))
}

/// The attributes an aggregator may count over for `type_name`: the published aggregatable
/// attributes of the one type Systems Manager documents as supporting aggregation
/// ([`INSTANCE_INFORMATION_AGGREGATABLE_ATTRIBUTES`]), and none for every other type — a
/// type this service reports no aggregatable schema for ([`reported_schemas`] with
/// `aggregatable_only`) supports no aggregation an expression could name.
#[must_use]
pub fn aggregatable_attributes(type_name: &str) -> &'static [&'static str] {
    if type_name == INVENTORY_TYPE_INSTANCE_INFORMATION {
        INSTANCE_INFORMATION_AGGREGATABLE_ATTRIBUTES
    } else {
        &[]
    }
}

/// Why an aggregator expression cannot be counted over: the three faults are three distinct
/// refusals, because the type name an expression carries and the attribute it names are two
/// different things to get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregatorRejection {
    /// The type-name part of the expression names no inventory type at all
    /// ([`is_known_inventory_type`]), which is a type-name fault rather than an aggregation
    /// one: it is stated by [`super::inventory_catalog::invalid_type_name_message`].
    UnknownTypeName(String),
    /// The expression names an attribute its type supports no aggregation on
    /// ([`aggregatable_attributes`]), stated by [`not_aggregatable_message`].
    AttributeNotAggregatable {
        /// The inventory type the expression names.
        type_name: String,
        /// The attribute of it the expression asks to count over.
        attribute: String,
    },
    /// The expression is not the `<TypeName>.<AttributeName>` form at all, so it names no
    /// aggregation this service could look up; stated by [`invalid_expression_message`].
    NotAnExpression(String),
}

/// How this service states a value that is not an aggregator expression: the expression the
/// request submitted, which is the only variable part.
#[must_use]
pub fn invalid_expression_message(expression: &str) -> String {
    format!("Invalid aggregator expression: {expression}.")
}

/// How this service states an expression naming an attribute its inventory type supports no
/// aggregation on: the attribute and the type the request named, and the aggregation that is
/// not supported for them.
///
/// Fixed text apart from the two names, which are echoed from the aggregator's own
/// expression.
#[must_use]
pub fn not_aggregatable_message(type_name: &str, attribute: &str) -> String {
    format!("Attribute {attribute} of type {type_name} does not support count aggregator.")
}

/// Why an aggregator expression cannot be counted over, or `None` when it can.
///
/// The expression is "the inventory type and attribute name for aggregation"
/// (`InventoryAggregator$Expression`): the inventory type up to the first `.` and the
/// attribute of it after that, so a value that is not that form names no aggregation to look
/// up at all. A value that IS that form carries a type name, held to the rule every inventory
/// type name of a request is held to ([`is_known_inventory_type`]), and an attribute, held to
/// the aggregatable attributes that type publishes ([`aggregatable_attributes`]) — three
/// faults, reported as three refusals.
pub fn aggregator_expression_rejection(expression: &str) -> Option<AggregatorRejection> {
    // A type name carries no `.` (a published name has none and a caller's own name is
    // alphanumeric past its prefix), so the first one separates the two parts.
    let (type_name, attribute) = expression.split_once('.').unwrap_or((expression, ""));
    if type_name.is_empty() || attribute.is_empty() {
        return Some(AggregatorRejection::NotAnExpression(expression.to_string()));
    }
    if !is_known_inventory_type(type_name) {
        return Some(AggregatorRejection::UnknownTypeName(type_name.to_string()));
    }
    if aggregatable_attributes(type_name).contains(&attribute) {
        return None;
    }
    Some(AggregatorRejection::AttributeNotAggregatable {
        type_name: type_name.to_string(),
        attribute: attribute.to_string(),
    })
}

/// The inventory types a cross-node inventory query can PROJECT, which is what a
/// `ResultAttributes` type name may name: the default type every managed node has
/// ("Valid value: `AWS:InstanceInformation`. Default Value: `AWS:InstanceInformation`" —
/// `ResultAttribute$TypeName`) and the patch summary of its last patch operation, the two
/// names the service's own refusal below lists.
///
/// It is NARROWER than the inventory types that EXIST ([`is_known_inventory_type`]): a type
/// outside this set is a real inventory type a read of one node's entries reports
/// (`ListInventoryEntries`) and no type this query projects, which is what
/// `InvalidResultAttributeException` reports ([`unsupported_result_type_message`]).
pub const SUPPORTED_RESULT_TYPE_NAMES: &[&str] =
    &[INVENTORY_TYPE_INSTANCE_INFORMATION, "AWS:PatchSummary"];

/// Whether `type_name` names a type this query projects ([`SUPPORTED_RESULT_TYPE_NAMES`]).
#[must_use]
pub fn is_supported_result_type(type_name: &str) -> bool {
    SUPPORTED_RESULT_TYPE_NAMES.contains(&type_name)
}

/// How this service states a `ResultAttributes` type name it cannot project: the name the
/// caller submitted, the names it may name instead, and the one it is served when it names
/// none.
///
/// Fixed text apart from the submitted name, which is echoed from the request.
#[must_use]
pub fn unsupported_result_type_message(type_name: &str) -> String {
    format!(
        "Result type name {type_name} is not supported. Supported type names are: {}. Default \
         result type name is {INVENTORY_TYPE_INSTANCE_INFORMATION}.",
        SUPPORTED_RESULT_TYPE_NAMES.join(", ")
    )
}

/// The published bounds of a schema read's page (`GetInventorySchemaMaxResults`, range
/// 50..200). The maximum is also the page a request naming no `MaxResults` is served: "the
/// maximum number of items to return for this call" bounds a page the caller asked to be
/// bounded, so a caller asking for no bound is served the whole page the operation allows.
pub const MIN_INVENTORY_SCHEMA_PAGE_SIZE: i32 = 50;

/// See [`MIN_INVENTORY_SCHEMA_PAGE_SIZE`].
pub const MAX_INVENTORY_SCHEMA_PAGE_SIZE: i32 = 200;

/// The page a schema read serves, or the refusal message for a `MaxResults` outside the
/// modelled range — which the boundary validator already refuses, so this is a backstop
/// rather than the first line of defence.
pub fn schema_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_INVENTORY_SCHEMA_PAGE_SIZE,
        MAX_INVENTORY_SCHEMA_PAGE_SIZE,
    )
}

/// The inventory schemas this endpoint reports for `region`, keyed by type name — the order
/// a schema read walks them in, which is what lets a page resume from the type name it
/// stopped at rather than from an offset.
///
/// Two sets make it up:
///
/// * every type Systems Manager PUBLISHES ([`super::inventory_catalog`]), with that type's
///   published attribute list and schema version. A published schema says what an account may
///   query, filter and aggregate on, which is why it is reported whether or not anything here
///   has collected an item of the type — the discovery call is what a client reads BEFORE it
///   has any data;
/// * every custom type the Region holds items of, whose schema is what those items say
///   ([`super::custom_inventory::custom_type_schema`]).
///
/// `aggregatable_only` reports the schemas that "support aggregation", with only the
/// attributes an aggregator may count over (`GetInventorySchemaRequest$Aggregator`): for the
/// one type whose aggregatable attributes this service knows that is
/// [`INSTANCE_INFORMATION_AGGREGATABLE_ATTRIBUTES`], and a custom type supports no
/// aggregation this service knows of, so none is reported.
pub fn reported_schemas(
    store: &super::ComAmazonawsSsmAmazonssmState,
    region: &str,
    aggregatable_only: bool,
) -> BTreeMap<String, InventoryItemSchema> {
    let mut schemas = BTreeMap::new();
    for published in super::inventory_catalog::PUBLISHED_INVENTORY_TYPES {
        // The attributes an aggregator may count over, for the one type whose aggregatable
        // attributes this service knows; a type it knows none of supports no aggregation it
        // could report, so an aggregation read leaves that type out.
        let aggregatable =
            Some(aggregatable_attributes(published.type_name)).filter(|set| !set.is_empty());
        let attributes = if aggregatable_only {
            match aggregatable {
                None => continue,
                Some(attributes) => attributes,
            }
        } else {
            published.attributes
        };
        schemas.insert(
            published.type_name.to_string(),
            published.to_schema(attributes),
        );
    }
    if aggregatable_only {
        return schemas;
    }
    let custom: BTreeSet<&str> = store.custom_inventory_types_in_region(region).collect();
    for type_name in custom {
        if let Some(schema) = super::custom_inventory::custom_type_schema(
            type_name,
            store.custom_inventory_of_type(region, type_name),
        ) {
            schemas.insert(type_name.to_string(), schema);
        }
    }
    schemas
}

/// One inventory item this endpoint holds about one node, as a read of that node's entries
/// reports it.
#[derive(Debug, Clone)]
pub struct HeldInventoryItem {
    /// "The inventory schema version used by the managed nodes."
    pub schema_version: String,
    /// "The time that inventory information was collected for the managed nodes", or `None`
    /// when nothing recorded it.
    pub capture_time: Option<String>,
    /// "A list of inventory items on the managed nodes": the entries the item carries.
    pub entries: Vec<HashMap<String, String>>,
}

/// The item `region` holds of `type_name` about the node `node_id`, or `None` when it holds
/// none.
///
/// The two kinds of item this endpoint holds, and nothing else:
///
/// * [`INVENTORY_TYPE_INSTANCE_INFORMATION`] is the PROJECTION of the registered node
///   ([`instance_information_content`]), so it exists exactly for a node the Region holds and
///   carries one entry — the node's own properties;
/// * a `Custom:` type is what a put wrote ([`super::custom_inventory`]), with the schema
///   version, the capture time and the entries that put supplied.
///
/// A default type this service publishes no schema of holds no item: nothing collects one
/// (see the module docs), so a read of it reports nothing rather than inventing it.
pub fn held_item(
    store: &super::ComAmazonawsSsmAmazonssmState,
    region: &str,
    node_id: &str,
    type_name: &str,
) -> Option<HeldInventoryItem> {
    if type_name == INVENTORY_TYPE_INSTANCE_INFORMATION {
        let node = store.managed_node(region, node_id)?;
        return Some(HeldInventoryItem {
            schema_version: INSTANCE_INFORMATION_SCHEMA_VERSION.to_string(),
            capture_time: capture_time(node),
            entries: vec![instance_information_content(node)],
        });
    }
    let item = store
        .custom_inventory_of_node(region, node_id)
        .find(|held| held.type_name == type_name)?;
    Some(HeldInventoryItem {
        schema_version: item.schema_version.clone(),
        capture_time: Some(item.capture_time.clone()),
        entries: item.content.clone(),
    })
}

/// Whether one inventory ENTRY satisfies one filter: the entry's value for the filter's key
/// stands in the filter's relation to one of its values.
///
/// The same rule [`matches_filter`] follows for a node's own item, over the entries of an
/// item instead: `Exists` asks only whether the attribute is reported, every other operator
/// is the comparison this service makes wherever it filters
/// ([`super::operator_matches`]), and an entry that reports no value for the attribute
/// satisfies no filter on it.
pub fn entry_matches(
    entry: &HashMap<String, String>,
    key: &str,
    values: &[String],
    operator: Option<&str>,
) -> bool {
    let Some(held) = entry.get(key) else {
        return false;
    };
    // The modelled operator defaults to `Equal`, the comparison a filter that names none
    // makes.
    let operator = operator.unwrap_or(super::OPERATOR_EQUAL);
    if operator == OPERATOR_EXISTS {
        return true;
    }
    values
        .iter()
        .any(|wanted| super::operator_matches(operator, held, wanted))
}

/// Why a filter key cannot be served by a read of ONE inventory type's entries, or `None`
/// when it can.
///
/// Such a filter names an attribute of the type the read already named, so the key is a bare
/// attribute name rather than the dotted form a cross-type query carries
/// ([`filter_key_rejection`]). For the one type whose schema this service publishes the
/// attribute has to be one of that schema's ([`INSTANCE_INFORMATION_ATTRIBUTES`]); for a
/// custom type the attributes are whatever its items carry, so any name is one that type may
/// publish and the filter is served — against the entries that hold it — rather than refused.
pub fn entry_filter_key_rejection(type_name: &str, key: &str) -> Option<&'static str> {
    let unknown_attribute = type_name == INVENTORY_TYPE_INSTANCE_INFORMATION
        && !INSTANCE_INFORMATION_ATTRIBUTES.contains(&key);
    (key.is_empty() || unknown_attribute).then_some(INVALID_FILTER)
}

/// The value `node` reports for one attribute of its `AWS:InstanceInformation` item, or
/// `None` when it reports none — either because the attribute is not one of that type's
/// ([`INSTANCE_INFORMATION_ATTRIBUTES`]) or because the node record holds no value for it.
///
/// Every value is a property of the registered node, so what an item reports is what the
/// node describes report: the item is a projection of the one node record rather than a
/// second copy of it. `InstanceStatus` is the node's own state
/// (`InstanceProperty$InstanceState`, "the current state of the node"), which is the
/// status `GetInventory` documents this type as carrying ("managed node status, such as
/// `Stopped` or `Terminated`").
pub fn instance_information_attribute<'a>(
    node: &'a ManagedNodeRecord,
    attribute: &str,
) -> Option<&'a str> {
    let properties = &node.properties;
    let value = match attribute {
        // Nothing records which agent implementation reported, so no node holds a value
        // for it (see [`ATTRIBUTE_AGENT_TYPE`]).
        ATTRIBUTE_AGENT_TYPE => &None,
        "AgentVersion" => &properties.agent_version,
        "ComputerName" => &properties.computer_name,
        "InstanceId" => &properties.instance_id,
        "InstanceStatus" => &properties.instance_state,
        "IpAddress" => &properties.ip_address,
        "PlatformName" => &properties.platform_name,
        "PlatformType" => &properties.platform_type,
        "PlatformVersion" => &properties.platform_version,
        "ResourceType" => &properties.resource_type,
        _ => return None,
    };
    value.as_deref()
}

/// The content of `node`'s `AWS:InstanceInformation` item: every attribute of that type
/// the node holds a value for, and no entry for one it does not — "Results include
/// attribute names and values" (`InventoryResultItem$Content`), and a node that has
/// reported no value for an attribute has no value to report.
pub fn instance_information_content(node: &ManagedNodeRecord) -> HashMap<String, String> {
    INSTANCE_INFORMATION_ATTRIBUTES
        .iter()
        .filter_map(|attribute| {
            instance_information_attribute(node, attribute)
                .map(|value| ((*attribute).to_string(), value.to_string()))
        })
        .collect()
}

/// When `node`'s `AWS:InstanceInformation` item was captured: the last time the node
/// reported to the service, and failing that the day it registered — "The time inventory
/// item data was captured" (`InventoryResultItem$CaptureTime`) is when the node last said
/// what it holds, so it comes from the node record rather than from the clock and two
/// reads of one node report the same instant. That instant is this service's one reading of
/// when a node last reported ([`super::node_listing::captured_at`], which a node listing
/// reports as its own `CaptureTime`).
///
/// Rendered as the modelled `InventoryItemCaptureTime` spells it (pattern
/// `^(20)[0-9][0-9]-…(Z)$`): whole seconds, so the instant is truncated to the second
/// rather than carrying the fraction a stored timestamp may hold.
pub fn capture_time(node: &ManagedNodeRecord) -> Option<String> {
    let stamped = super::node_listing::captured_at(node)?;
    ::aws_smithy_types::DateTime::from_secs(stamped.secs())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .ok()
}

/// Whether `node` satisfies one inventory filter: the filter's key names an attribute of
/// an inventory type, and the node's value for it stands in the filter's relation to one
/// of the filter's values.
///
/// `Exists` asks only whether the attribute is reported, which is what makes it the one
/// operator a node answers without comparing anything; every other operator is the
/// comparison the node describes already make ([`super::operator_matches`]), so one rule
/// orders a version and compares a name wherever this service filters. A node that holds
/// no value for the attribute — including every node for an inventory type nothing has
/// collected — satisfies NO filter on it, whatever the operator: a filter selects the
/// nodes whose attribute satisfies it, and a node without the attribute has nothing to
/// satisfy it with.
pub fn matches_filter(
    node: &ManagedNodeRecord,
    key: &str,
    values: &[String],
    operator: Option<&str>,
) -> bool {
    let Some((type_name, attribute)) = split_qualified_attribute(key) else {
        return false;
    };
    if type_name != INVENTORY_TYPE_INSTANCE_INFORMATION {
        // A type this store holds no item of: the node reports no attribute of it.
        return false;
    }
    let Some(held) = instance_information_attribute(node, attribute) else {
        return false;
    };
    // The modelled operator defaults to `Equal`, the comparison a filter that names none
    // makes ("Equal" is the relation a key and a value state on their own).
    let operator = operator.unwrap_or(super::OPERATOR_EQUAL);
    if operator == OPERATOR_EXISTS {
        return true;
    }
    values
        .iter()
        .any(|wanted| super::operator_matches(operator, held, wanted))
}
