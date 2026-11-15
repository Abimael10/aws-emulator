// SPDX-License-Identifier: CC0-1.0

//! The `aws-auth` ConfigMap: the mapping a cluster authenticates IAM principals
//! through when its `authenticationMode` includes `CONFIG_MAP`.
//!
//! Before access entries, and still for every cluster whose mode includes
//! `CONFIG_MAP`, a cluster's Kubernetes API server resolves an IAM principal
//! through a ConfigMap named `aws-auth` in `kube-system`: its `mapRoles` and
//! `mapUsers` values are YAML lists, each entry naming an IAM principal
//! (`rolearn` / `userarn`), the `username` that principal authenticates as and
//! the `groups` it is a member of — the same three things an access entry
//! carries ([`super::AccessEntryRecord`]), which is why "access entries can
//! replace the need to maintain entries in the aws-auth ConfigMap"
//! (`CreateAccessEntry`).
//!
//! So the mapping is read from the cluster's OWN object: whatever a client wrote
//! into that ConfigMap through the published API server address is what
//! authenticates, and a cluster whose ConfigMap holds no entry for a principal
//! maps it to nobody. Nothing here is derived from the request's own text.

use super::kubernetes_objects::ClusterObjects;

/// The namespace Amazon EKS keeps the mapping in.
pub const AWS_AUTH_NAMESPACE: &str = "kube-system";

/// The name of the ConfigMap it is kept under.
pub const AWS_AUTH_CONFIG_MAP: &str = "aws-auth";

/// The `data` key whose YAML list maps IAM ROLES.
pub const AWS_AUTH_MAP_ROLES: &str = "mapRoles";

/// The `data` key whose YAML list maps IAM USERS.
pub const AWS_AUTH_MAP_USERS: &str = "mapUsers";

/// The member of a `mapRoles` entry that names the IAM role it maps.
const MAP_ROLES_PRINCIPAL_MEMBER: &str = "rolearn";

/// The member of a `mapUsers` entry that names the IAM user it maps.
const MAP_USERS_PRINCIPAL_MEMBER: &str = "userarn";

/// The member of either entry that names the Kubernetes user the principal
/// authenticates as.
const MAPPING_USERNAME_MEMBER: &str = "username";

/// The member of either entry that names the Kubernetes groups it is in.
const MAPPING_GROUPS_MEMBER: &str = "groups";

/// How many mappings ONE list is read from.
///
/// CHOSEN, not published: the mapping is a short list a cluster's administrators
/// maintain, and this read runs on every request to the cluster's API server, so
/// what one request costs must not grow with what a caller wrote into the
/// ConfigMap. It is far above the handful of mappings a cluster of this emulator
/// is driven with.
pub const MAX_AWS_AUTH_MAPPINGS: usize = 64;

/// How many groups ONE mapping is read with, bounded for the same reason.
pub const MAX_AWS_AUTH_MAPPING_GROUPS: usize = 64;

/// How many bytes of one list are read as a mapping.
///
/// CHOSEN for the same reason as [`MAX_AWS_AUTH_MAPPINGS`]: a ConfigMap value may
/// carry up to [`super::kubernetes_config_maps::MAX_CONFIG_MAP_BYTES`], and
/// parsing that much YAML per request is work no request to this address needs. A
/// value past this is not a mapping list this server reads.
pub const MAX_AWS_AUTH_MAPPING_BYTES: usize = 64 * 1024;

/// How deeply nested a mapping list is read: a list of mappings whose groups are
/// a list of scalars is three levels, and this is above it with room to spare.
/// Bounded because a YAML loader builds the whole tree it is given.
pub const MAX_AWS_AUTH_MAPPING_DEPTH: usize = 8;

/// One entry of the mapping: an IAM principal, the Kubernetes user it
/// authenticates as and the groups it is a member of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsAuthMapping {
    /// The IAM principal ARN the entry names, exactly as it was written.
    pub principal_arn: String,
    /// The username it authenticates as, as written — it may carry the
    /// placeholders Amazon EKS resolves per request
    /// ([`super::access_entry_usernames`]).
    pub username: String,
    /// The Kubernetes groups it is a member of, in the order the entry lists
    /// them.
    pub groups: Vec<String>,
}

impl ClusterObjects {
    /// The mapping this cluster's `aws-auth` ConfigMap holds for
    /// `principal_arn`, or `None` when it holds none — a cluster with no such
    /// ConfigMap among them.
    ///
    /// The ROLE list is read before the user list, which is the order the
    /// ConfigMap declares them in; an ARN appears in one of the two, so the
    /// order only decides which entry wins for a ConfigMap that names the same
    /// ARN twice — the first one written, as Kubernetes reads a list.
    #[must_use]
    pub fn aws_auth_mapping(&self, principal_arn: &str) -> Option<AwsAuthMapping> {
        let config_map = self
            .namespaces
            .get(AWS_AUTH_NAMESPACE)?
            .config_maps
            .get(AWS_AUTH_CONFIG_MAP)?;
        [
            (AWS_AUTH_MAP_ROLES, MAP_ROLES_PRINCIPAL_MEMBER),
            (AWS_AUTH_MAP_USERS, MAP_USERS_PRINCIPAL_MEMBER),
        ]
        .into_iter()
        .find_map(|(key, member)| {
            let text = config_map.data.get(key)?;
            aws_auth_mappings(text, member)
                .into_iter()
                .find(|mapping| mapping.principal_arn == principal_arn)
        })
    }
}

/// The mappings one `data` value states, read as the YAML list the ConfigMap
/// carries, with `principal_member` naming the IAM principal of an entry.
///
/// A value this server does not read as a list of mappings — one past
/// [`MAX_AWS_AUTH_MAPPING_BYTES`] or [`MAX_AWS_AUTH_MAPPING_DEPTH`], one that
/// references a YAML anchor (a loader resolves an alias by COPYING the anchored
/// node into every alias position, so a shallow document can expand
/// multiplicatively while it is being loaded), one that is not YAML at all, one
/// that is not a list — states no mapping, so the principals it would have named
/// authenticate as nobody rather than as something guessed at. An entry missing
/// the principal or the username names no mapping either and is left out; an entry
/// with no `groups` maps a principal to a username in no group, which is what the
/// member being absent means.
#[must_use]
pub fn aws_auth_mappings(text: &str, principal_member: &str) -> Vec<AwsAuthMapping> {
    if text.len() > MAX_AWS_AUTH_MAPPING_BYTES
        || ::service_common::yaml_nesting::nesting_depth_bound(text, MAX_AWS_AUTH_MAPPING_DEPTH)
            > MAX_AWS_AUTH_MAPPING_DEPTH
        || super::addon_catalog::references_an_anchor(text)
    {
        return Vec::new();
    }
    let Ok(documents) = ::yaml_rust2::YamlLoader::load_from_str(text) else {
        return Vec::new();
    };
    let Some(::yaml_rust2::Yaml::Array(entries)) = documents.first() else {
        return Vec::new();
    };
    entries
        .iter()
        .take(MAX_AWS_AUTH_MAPPINGS)
        .filter_map(|entry| {
            let principal_arn = scalar(entry, principal_member)?;
            let username = scalar(entry, MAPPING_USERNAME_MEMBER)?;
            Some(AwsAuthMapping {
                principal_arn,
                username,
                groups: groups_of(entry),
            })
        })
        .collect()
}

/// The text of a scalar member of one mapping entry, or `None` when the entry
/// carries no such member or carries it as something other than a non-empty
/// scalar.
fn scalar(entry: &::yaml_rust2::Yaml, member: &str) -> Option<String> {
    let value = entry[member].as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The groups one mapping entry lists: its scalar entries, in order, bounded by
/// [`MAX_AWS_AUTH_MAPPING_GROUPS`]. Empty for an entry that lists none.
fn groups_of(entry: &::yaml_rust2::Yaml) -> Vec<String> {
    let Some(listed) = entry[MAPPING_GROUPS_MEMBER].as_vec() else {
        return Vec::new();
    };
    listed
        .iter()
        .take(MAX_AWS_AUTH_MAPPING_GROUPS)
        .filter_map(|group| {
            let group = group.as_str()?.trim();
            (!group.is_empty()).then(|| group.to_string())
        })
        .collect()
}
