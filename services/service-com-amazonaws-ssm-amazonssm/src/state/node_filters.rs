// SPDX-License-Identifier: CC0-1.0

//! The string filters a MANAGED-NODE read accepts: the keys
//! `DescribeInstanceInformation`'s `Filters` publishes, the shapes an activation id and an
//! agent version have, and why a filter value is not one the attribute it filters could
//! hold.
//!
//! The same managed nodes the property describe reaches, through the filter vocabulary
//! `DescribeInstanceInformation`'s own `Filters` member publishes — one concern beside the
//! node modules that hold the nodes themselves ([`super::node_registration`],
//! [`super::ec2_managed_nodes`]). Every name stays reachable as `crate::state::<name>`
//! through [`super`]'s re-export.

use super::*;

/// The filter keys `DescribeInstanceInformation`'s `Filters` accepts beside the two tag
/// forms: "Valid filter key values: ActivationIds | AgentVersion | AssociationStatus |
/// IamRole | InstanceIds | PingStatus | PlatformTypes | ResourceType | SourceIds |
/// SourceTypes | "tag-key" | "tag:{keyname}"" (the `InstanceInformationStringFilter$Key`
/// doc). It is the property describe's set ([`is_node_filter_key`]) without
/// `DocumentName`, which that operation's filter publishes and this one's does not.
pub const INSTANCE_INFORMATION_FILTER_KEYS: &[&str] = &[
    "ActivationIds",
    "AgentVersion",
    "AssociationStatus",
    "IamRole",
    "InstanceIds",
    "PingStatus",
    "PlatformTypes",
    "ResourceType",
    "SourceIds",
    "SourceTypes",
];

/// Whether `key` names a tag rather than a property of a node ([`TAG_KEY_FILTER_KEY`] or
/// a [`TAG_FILTER_KEY_PREFIX`] key).
pub fn is_tag_filter_key(key: &str) -> bool {
    key == TAG_KEY_FILTER_KEY || key.starts_with(TAG_FILTER_KEY_PREFIX)
}

/// Whether `key` is a filter key `DescribeInstanceInformation`'s `Filters` documents.
///
/// `tag:` must name a tag key after the colon, so `tag:` alone is not one. The
/// comparison is exact, as it is for the property describe: a filter key is a name, so
/// neither a different spelling nor a different case names a property of a node.
pub fn is_instance_information_filter_key(key: &str) -> bool {
    if let Some(tag_key) = key.strip_prefix(TAG_FILTER_KEY_PREFIX) {
        return !tag_key.is_empty();
    }
    key == TAG_KEY_FILTER_KEY || INSTANCE_INFORMATION_FILTER_KEYS.contains(&key)
}

/// Whether `key` is a key the legacy `InstanceInformationFilterList` accepts — the
/// modelled `InstanceInformationFilterKey` enum, which carries no tag form ("The
/// `InstanceInformationFilterList` method is a legacy method and doesn't support tags").
pub fn is_legacy_node_filter_key(key: &str) -> bool {
    INSTANCE_INFORMATION_FILTER_KEY_VALUES.contains(&key)
}

/// Whether `id` is an activation id — the modelled `ActivationId` pattern
/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`.
pub fn is_activation_id(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != width || !group.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return false;
        }
    }
    groups.next().is_none()
}

/// The filter key a managed-node describe selects on the activation a node registered
/// with — one of the modelled `InstancePropertyFilterKey` values, and one of the keys
/// `InstanceInformationStringFilter$Key` publishes.
pub const ACTIVATION_IDS_FILTER_KEY: &str = "ActivationIds";

/// Which values of an [`ACTIVATION_IDS_FILTER_KEY`] filter are not activation ids, in the
/// order the request listed them ([`is_activation_id`] is this crate's one reading of
/// that shape).
///
/// A value a filter may be served with has to be a value the member it filters can hold,
/// and an activation id is a member the model constrains by pattern — so a value outside
/// it names no activation that could ever exist, which is a different answer from a
/// well-formed id no node has registered with.
pub fn malformed_activation_ids(values: &[String]) -> Vec<&str> {
    values
        .iter()
        .map(String::as_str)
        .filter(|value| !is_activation_id(value))
        .collect()
}

/// How a describe refuses the activation-id filter values that are not activation ids:
/// the fixed text plus the rejected values themselves, taken from the request in the
/// order it listed them.
pub fn invalid_activation_ids_message(rejected: &[&str]) -> String {
    format!("Invalid Activation Ids {}", rejected.join(", "))
}

/// Whether `version` is an SSM Agent version — the modelled `Version` pattern
/// `^[0-9]{1,6}(\.[0-9]{1,6}){2,3}$`: three or four dot-separated runs of 1..6 digits.
pub fn is_agent_version(version: &str) -> bool {
    let groups: Vec<&str> = version.split('.').collect();
    (3..=4).contains(&groups.len())
        && groups
            .iter()
            .all(|group| (1..=6).contains(&group.len()) && group.bytes().all(|b| b.is_ascii_digit()))
}

/// Why `value` is not a value the node attribute `key` filters could hold, or `None`
/// when it could be one.
///
/// The rule is one sentence: a filter value has to be a legal value of the member it
/// filters, as the model constrains that member — one of its enum's values
/// ([`node_filter_key_value_set`]), or a string matching its pattern. A key whose member
/// the model leaves unconstrained (`IamRole`, `SourceIds`, a tag) constrains nothing
/// here, and a node id is checked by its caller, which answers it with the operation's
/// own `InvalidInstanceId` rather than with a filter-value error.
///
/// An [`ACTIVATION_IDS_FILTER_KEY`] value is worded the way the service words that
/// refusal ([`invalid_activation_ids_message`]); an operation whose model declares the
/// activation-id error answers THAT with the same sentence rather than the filter-value
/// error, which is why the wording is held in one place.
pub fn node_filter_value_rejection(key: &str, value: &str) -> Option<String> {
    if let Some(catalog) = node_filter_key_value_set(key) {
        if !catalog.contains(&value) {
            return Some(node_filter_value_set_rejection(key, catalog));
        }
        return None;
    }
    match key {
        ACTIVATION_IDS_FILTER_KEY if !is_activation_id(value) => {
            Some(invalid_activation_ids_message(&[value]))
        }
        "AgentVersion" if !is_agent_version(value) => Some(format!(
            "Value '{value}' for filter key '{key}' is not an SSM Agent version"
        )),
        _ => None,
    }
}
