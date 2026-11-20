// SPDX-License-Identifier: CC0-1.0

//! The inventory types Systems Manager PUBLISHES, and the naming rule a type name has to
//! satisfy to name an inventory type at all.
//!
//! `GetInventorySchema` is the discovery call: "return a list of inventory type names for the
//! account, or return a list of attribute names for a specific Inventory item type", and the
//! names it reports are the catalog Systems Manager publishes — "Default inventory item type
//! names start with Amazon Web Services. … Default inventory item types include the
//! following: `AWS:AWSComponent`, `AWS:Application`, `AWS:InstanceInformation`, `AWS:Network`,
//! and `AWS:WindowsUpdate`" (`InventoryItemSchema$TypeName`), of which that doc names five of
//! the fifteen. The catalog below is that published set: one entry per type, with the
//! attribute list and the schema version Systems Manager publishes for it (AWS Systems Manager
//! User Guide, "Metadata collected by Systems Manager Inventory", which tabulates each type's
//! attributes; `AWS:InstanceInformation`'s own attributes are the ones a node's inventory item
//! carries).
//!
//! A catalog entry is INDEPENDENT of what this endpoint has collected: the schemas are what an
//! account may query, filter and aggregate on, and they are published whether or not any node
//! has reported an item of the type. What a type has collected is a different question, which
//! [`super::inventory::held_item`] answers — `AWS:InstanceInformation` is the projection of the
//! node record and a `Custom:` type is what a put wrote ([`super::custom_inventory`]), and
//! every other published type has collected nothing here.
//!
//! PROVISIONAL ENTRIES: the attribute lists of `AWS:AWSComponent`, `AWS:Application` and
//! `AWS:InstanceInformation` are the published ones, confirmed against a recorded schema read.
//! The lists of `AWS:ComplianceItem`, `AWS:ComplianceSummary`, `AWS:File`,
//! `AWS:InstanceDetailedInformation`, `AWS:Network`, `AWS:PatchSummary` and `AWS:WindowsRole` are
//! reconstructed from the same published documentation and are NOT confirmed against a recorded
//! read, so each of their consts says so: a later correction is a one-line change to the list it
//! names, and nothing else here depends on their contents.
//!
//! The NAMING rule is this module's other half, because the catalog is what makes it decidable:
//! a type name is either one of the published names, spelled exactly — "Type name is case
//! sensitive" — or a name of the caller's own namespace matching
//! [`CUSTOM_TYPE_NAME_PATTERN`]. Anything else names no inventory type, which is the modelled
//! `InvalidTypeNameException` ([`invalid_type_name_message`]).

use crate::ops::types::{InventoryItemAttribute, InventoryItemSchema};

use super::inventory::CUSTOM_TYPE_PREFIX;

/// One inventory type Systems Manager publishes a schema of.
#[derive(Debug, Clone, Copy)]
pub struct PublishedInventoryType {
    /// "The name of the inventory type" (`InventoryItemSchema$TypeName`), of the `AWS:`
    /// namespace Systems Manager publishes under.
    pub type_name: &'static str,
    /// "The schema version for the inventory item" (`InventoryItemSchema$Version`), in the
    /// `^([0-9]{1,6})(\.[0-9]{1,6})$` form that member spells.
    pub version: &'static str,
    /// "The schema attributes for inventory", in the order the published schema lists them.
    pub attributes: &'static [&'static str],
}

impl PublishedInventoryType {
    /// This type as the modelled `InventoryItemSchema` reports it, carrying `attributes`
    /// rather than all of its own — which is how an aggregation read reports only the
    /// attributes it may count over ([`super::inventory::reported_schemas`]).
    ///
    /// Every attribute of a published type carries the TEXT data type. That is RECORDED
    /// BEHAVIOUR rather than an assumption about the values: a schema read of the published
    /// catalog answers `"DataType": "STRING"` for every attribute of every type, the ones
    /// whose values read as numbers (a count, a version, a size) included. A custom type is
    /// the other case — its attributes are whatever its items say, so
    /// [`super::custom_inventory::custom_type_schema`] reports the number type where every
    /// value under an attribute is one.
    ///
    /// No `DisplayName`: "the alias name of the inventory type … used for display purposes",
    /// and Systems Manager publishes these types under their own names, so none is invented.
    pub fn to_schema(&self, attributes: &[&str]) -> InventoryItemSchema {
        InventoryItemSchema {
            type_name: self.type_name.to_string(),
            version: Some(self.version.to_string()),
            attributes: attributes
                .iter()
                .map(|name| InventoryItemAttribute {
                    name: (*name).to_string(),
                    data_type: super::custom_inventory::ATTRIBUTE_DATA_TYPE_STRING.to_string(),
                })
                .collect(),
            display_name: None,
        }
    }
}

/// The attributes of `AWS:AWSComponent`: the Amazon Web Services components installed on a
/// node, reported with the same properties an application is.
const AWS_COMPONENT_ATTRIBUTES: &[&str] = &[
    "Name",
    "ApplicationType",
    "Publisher",
    "Version",
    "InstalledTime",
    "Architecture",
    "URL",
];

/// The attributes of `AWS:Application`: the applications installed on a node, with the
/// packaging members (`Release`, `Epoch`, `PackageId`) the second revision of that schema
/// added.
const APPLICATION_ATTRIBUTES: &[&str] = &[
    "Name",
    "ApplicationType",
    "Publisher",
    "Version",
    "Release",
    "Epoch",
    "InstalledTime",
    "Architecture",
    "URL",
    "Summary",
    "PackageId",
];

/// The attributes of `AWS:ComplianceItem`: one compliance finding about a node — which
/// compliance type reported it, the execution that produced it, and the finding's own
/// identity, status and severity, with the detail a patch or association finding carries.
/// PROVISIONAL past the first eight (see the module docs).
const COMPLIANCE_ITEM_ATTRIBUTES: &[&str] = &[
    "ComplianceType",
    "ExecutionId",
    "ExecutionType",
    "ExecutionTime",
    "Id",
    "Title",
    "Status",
    "Severity",
    "DocumentName",
    "DocumentVersion",
    "Classification",
    "PatchBaselineId",
    "PatchSeverity",
    "PatchState",
    "PatchGroup",
    "InstalledTime",
    "CVEIds",
];

/// The attributes of `AWS:ComplianceSummary`: the roll-up of a node's compliance findings —
/// the execution that produced them, the overall status, and the compliant and non-compliant
/// counts per severity. PROVISIONAL (see the module docs).
const COMPLIANCE_SUMMARY_ATTRIBUTES: &[&str] = &[
    "ComplianceType",
    "Status",
    "OverallSeverity",
    "ExecutionId",
    "ExecutionType",
    "ExecutionTime",
    "CompliantCriticalCount",
    "CompliantHighCount",
    "CompliantMediumCount",
    "CompliantLowCount",
    "CompliantInformationalCount",
    "CompliantUnspecifiedCount",
    "NonCompliantCriticalCount",
    "NonCompliantHighCount",
    "NonCompliantMediumCount",
    "NonCompliantLowCount",
    "NonCompliantInformationalCount",
    "NonCompliantUnspecifiedCount",
];

/// The attributes of `AWS:File`: one file the collection was configured to look for, with the
/// file's own metadata and the product it belongs to. PROVISIONAL (see the module docs).
const FILE_ATTRIBUTES: &[&str] = &[
    "Name",
    "Size",
    "Description",
    "FileVersion",
    "InstalledDate",
    "ModificationTime",
    "LastAccessTime",
    "ProductName",
    "InstalledDir",
    "ProductLanguage",
    "ProductVersion",
    "CompanyName",
];

/// The attributes of `AWS:InstanceDetailedInformation`: the node's CPU and kernel detail,
/// beside the identity `AWS:InstanceInformation` carries. PROVISIONAL (see the module docs).
const INSTANCE_DETAILED_INFORMATION_ATTRIBUTES: &[&str] = &[
    "CPUModel",
    "CPUCores",
    "CPUs",
    "CPUSpeedMHz",
    "CPUSockets",
    "CPUHyperThreadEnabled",
    "OSServicePack",
    "KernelVersion",
];

/// The attributes of `AWS:InstanceInformation`: the registered node itself — the agent it
/// runs, the platform it runs on, its identity and address, the role it assumes, its status,
/// where it was registered from and which Availability Zone holds it.
pub const INSTANCE_INFORMATION_ATTRIBUTES: &[&str] = &[
    "AgentType",
    "AgentVersion",
    "ComputerName",
    "IamRole",
    "InstanceId",
    "IpAddress",
    "PlatformName",
    "PlatformType",
    "PlatformVersion",
    "ResourceType",
    "AgentStatus",
    "InstanceStatus",
    "SourceType",
    "SourceId",
    "SourceLocation",
    "AvailabilityZone",
    "AvailabilityZoneId",
];

/// The attributes of `AWS:Network`: one network interface of the node, with its addresses and
/// the servers it was configured with. PROVISIONAL (see the module docs).
const NETWORK_ATTRIBUTES: &[&str] = &[
    "Name",
    "SubnetMask",
    "Gateway",
    "DHCPServer",
    "DNSServer",
    "MacAddress",
    "IPV4",
    "IPV6",
];

/// The attributes of `AWS:PatchSummary`: the result of the last patch operation on the node —
/// the baseline and snapshot it ran against, the counts in each patch state, and when it ran.
/// PROVISIONAL (see the module docs).
const PATCH_SUMMARY_ATTRIBUTES: &[&str] = &[
    "PatchGroup",
    "BaselineId",
    "SnapshotId",
    "OwnerInformation",
    "InstalledCount",
    "InstalledOtherCount",
    "InstalledPendingRebootCount",
    "InstalledRejectedCount",
    "MissingCount",
    "FailedCount",
    "UnreportedNotApplicableCount",
    "NotApplicableCount",
    "OperationStartTime",
    "OperationEndTime",
    "OperationType",
    "RebootOption",
    "CriticalNonCompliantCount",
    "SecurityNonCompliantCount",
    "OtherNonCompliantCount",
    "ExecutionId",
    "ExecutionType",
    "ExecutionTime",
];

/// The attributes of `AWS:ResourceGroup`: the resource group the node belongs to.
const RESOURCE_GROUP_ATTRIBUTES: &[&str] = &["Name", "Arn"];

/// The attributes of `AWS:Service`: one service installed on the node, its state, and the
/// services it depends on either way.
const SERVICE_ATTRIBUTES: &[&str] = &[
    "Name",
    "DisplayName",
    "ServiceType",
    "Status",
    "DependentServices",
    "ServicesDependedOn",
    "StartType",
];

/// The attributes of `AWS:Tag`: one tag of the node, as a key and a value.
const TAG_ATTRIBUTES: &[&str] = &["Key", "Value"];

/// The attributes of `AWS:WindowsRegistry`: one registry value the collection was configured
/// to read, named by the key path it lives under.
const WINDOWS_REGISTRY_ATTRIBUTES: &[&str] = &["KeyPath", "ValueName", "ValueType", "Value"];

/// The attributes of `AWS:WindowsRole`: one Windows role or feature, whether it is installed,
/// and where it sits in the role tree. PROVISIONAL (see the module docs).
const WINDOWS_ROLE_ATTRIBUTES: &[&str] = &[
    "Name",
    "DisplayName",
    "Description",
    "Installed",
    "InstalledState",
    "SubType",
    "Path",
    "ParentName",
    "ParentDisplayName",
];

/// The attributes of `AWS:WindowsUpdate`: one update installed on the node, by hotfix id.
const WINDOWS_UPDATE_ATTRIBUTES: &[&str] = &[
    "HotFixId",
    "Description",
    "InstalledTime",
    "InstalledBy",
];

/// The inventory types Systems Manager publishes, in ascending type-name order — the order a
/// schema read walks the catalog in, which is what lets a page resume from the type name it
/// stopped at (see the module docs for where the set comes from).
pub const PUBLISHED_INVENTORY_TYPES: &[PublishedInventoryType] = &[
    PublishedInventoryType {
        type_name: "AWS:AWSComponent",
        version: "1.0",
        attributes: AWS_COMPONENT_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:Application",
        version: "1.1",
        attributes: APPLICATION_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:ComplianceItem",
        version: "1.0",
        attributes: COMPLIANCE_ITEM_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:ComplianceSummary",
        version: "1.0",
        attributes: COMPLIANCE_SUMMARY_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:File",
        version: "1.0",
        attributes: FILE_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:InstanceDetailedInformation",
        version: "1.0",
        attributes: INSTANCE_DETAILED_INFORMATION_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: super::inventory::INVENTORY_TYPE_INSTANCE_INFORMATION,
        version: "1.1",
        attributes: INSTANCE_INFORMATION_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:Network",
        version: "1.0",
        attributes: NETWORK_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:PatchSummary",
        version: "1.0",
        attributes: PATCH_SUMMARY_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:ResourceGroup",
        version: "1.0",
        attributes: RESOURCE_GROUP_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:Service",
        version: "1.0",
        attributes: SERVICE_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:Tag",
        version: "1.0",
        attributes: TAG_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:WindowsRegistry",
        version: "1.0",
        attributes: WINDOWS_REGISTRY_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:WindowsRole",
        version: "1.0",
        attributes: WINDOWS_ROLE_ATTRIBUTES,
    },
    PublishedInventoryType {
        type_name: "AWS:WindowsUpdate",
        version: "1.0",
        attributes: WINDOWS_UPDATE_ATTRIBUTES,
    },
];

/// The published type `type_name` names, or `None` when it names none — an exact comparison,
/// because "Type name is case sensitive" (see [`invalid_type_name_message`]): neither another
/// case nor another spelling names a published type.
#[must_use]
pub fn published_type(type_name: &str) -> Option<&'static PublishedInventoryType> {
    PUBLISHED_INVENTORY_TYPES
        .iter()
        .find(|published| published.type_name == type_name)
}

/// Whether `type_name` names one of the types Systems Manager publishes.
#[must_use]
pub fn is_published_type(type_name: &str) -> bool {
    published_type(type_name).is_some()
}

/// The pattern a name of the caller's own inventory namespace has to satisfy, as the service
/// states it in the refusal below: `Custom:` followed by 3 to 93 alphanumeric characters.
pub const CUSTOM_TYPE_NAME_PATTERN: &str = "^(Custom:)([a-zA-Z0-9]{3,93})$";

/// Whether `type_name` satisfies [`CUSTOM_TYPE_NAME_PATTERN`]: the `Custom:` prefix and then
/// 3..=93 alphanumeric characters, so a custom name carrying a separator — a hyphen, a dot, an
/// underscore — satisfies it no more than an empty one does.
#[must_use]
pub fn is_valid_custom_type_name(type_name: &str) -> bool {
    let Some(name) = type_name.strip_prefix(CUSTOM_TYPE_PREFIX) else {
        return false;
    };
    // Every admitted character is one ASCII byte, so the byte length is the character count
    // the pattern bounds.
    (3..=93).contains(&name.len()) && name.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// The rules a type name has to satisfy, as this service states them in every refusal of
/// one: the published names are matched case-sensitively, every name belongs to one of the
/// two namespaces, and a name of the caller's own namespace matches
/// [`CUSTOM_TYPE_NAME_PATTERN`].
///
/// Fixed text, and the whole of it is shared by the refusals below: what differs between them
/// is only which value they say is invalid.
fn type_name_rules() -> String {
    format!(
        "Type name is case sensitive, such as AWS:APpLICation is invalid; Type name must start \
         with either AWS: or Custom:;  Custom inventory type name must satisfy regular \
         expression pattern: {CUSTOM_TYPE_NAME_PATTERN}"
    )
}

/// How this service refuses a type name that names no inventory type: the name the caller
/// submitted, and the rules a name has to satisfy ([`type_name_rules`]).
///
/// Fixed text apart from the bracketed name, which is echoed from the request.
#[must_use]
pub fn invalid_type_name_message(type_name: &str) -> String {
    format!(
        "Inventory type name [{type_name}] is invalid. {}",
        type_name_rules()
    )
}

/// How this service refuses a FILTER KEY whose type-name part names no inventory type: the
/// type name and the whole key the caller submitted, and the same rules a name has to satisfy
/// ([`type_name_rules`]) — the fault is the type name, so a key carrying one is refused as the
/// type-name fault it is, naming the key it was read from.
///
/// Fixed text apart from the two bracketed values, both echoed from the request.
#[must_use]
pub fn invalid_filter_key_type_name_message(type_name: &str, key: &str) -> String {
    format!(
        "Type name [{type_name}] of filter key [{key}] is invalid.{}",
        type_name_rules()
    )
}
