// SPDX-License-Identifier: CC0-1.0

//! The `metadata` of a Kubernetes object a client created through a cluster's
//! published API server address: the members the CLIENT submitted, and the
//! `metadata` document every object of this plane is rendered with.
//!
//! A create is answered with the object the server STORED, so the labels and
//! annotations the manifest carried are kept on the record and reported back by
//! the create and by every later read — a client that labels a workload and then
//! selects on that label reads its own label back. The members the server itself
//! owns (the name, the namespace, the minted `uid`, the creation instant and the
//! revision the write that stored the object was stamped with) are rendered beside
//! them from the record rather than from the manifest.
//!
//! One renderer for every object kind of [`super::kubernetes_objects`], so a
//! Namespace, a ServiceAccount, a Pod and a ConfigMap cannot disagree about what
//! an object's `metadata` looks like.

use super::kubernetes_objects::ApiRefusal;
use super::*;

/// The `metadata` members a client submits with an object and reads back on it.
///
/// Labels and annotations are string-to-string maps in the Kubernetes API, so a
/// member holding anything else is a manifest the API server cannot store and is
/// REFUSED rather than stored with that entry dropped — a client that reads the
/// object back finds every label it sent. There is no count bound of its own: a
/// manifest is read under the per-request byte ceiling the served address applies
/// to every body, which is what bounds how many of these one object can carry.
#[derive(Debug, Default, Clone)]
pub struct ObjectMetadata {
    /// `metadata.labels`: the identifying key/value pairs a selector matches on.
    pub labels: BTreeMap<String, String>,
    /// `metadata.annotations`: the non-identifying key/value pairs a client and
    /// its tooling keep on an object.
    pub annotations: BTreeMap<String, String>,
}

impl ObjectMetadata {
    /// The labels and annotations `manifest` carries, as the object of kind
    /// `kind` named `name` will hold them. A manifest with no `metadata`, or one
    /// that names neither member, holds neither.
    pub fn from_manifest(
        kind: &str,
        name: &str,
        manifest: &::serde_json::Value,
    ) -> Result<Self, ApiRefusal> {
        let metadata = manifest.get("metadata");
        Ok(Self {
            labels: string_value_map(
                kind,
                name,
                "metadata.labels",
                metadata.and_then(|metadata| metadata.get("labels")),
            )?,
            annotations: string_value_map(
                kind,
                name,
                "metadata.annotations",
                metadata.and_then(|metadata| metadata.get("annotations")),
            )?,
        })
    }

    /// This object's `metadata` as a client reads it: what the server owns (the
    /// name, the namespace of a namespaced object, the uid it minted, the instant
    /// it created the object and the revision `resource_version` of the write that
    /// last stored it) and what the client submitted.
    ///
    /// An empty label or annotation map is left out rather than reported as an
    /// empty object, which is how the API server reports an object carrying
    /// none.
    #[must_use]
    pub fn render(
        &self,
        name: &str,
        namespace: Option<&str>,
        uid: &str,
        created_at: SystemTime,
        resource_version: u64,
    ) -> ::serde_json::Value {
        let mut metadata = ::serde_json::Map::new();
        metadata.insert("name".to_string(), ::serde_json::json!(name));
        if let Some(namespace) = namespace {
            metadata.insert("namespace".to_string(), ::serde_json::json!(namespace));
        }
        metadata.insert("uid".to_string(), ::serde_json::json!(uid));
        // The revision this object stands at, which a client hands back to write
        // it ([`super::kubernetes_resource_versions`]).
        metadata.insert(
            "resourceVersion".to_string(),
            ::serde_json::json!(kubernetes_resource_versions::rendered(resource_version)),
        );
        metadata.insert(
            "creationTimestamp".to_string(),
            ::serde_json::json!(kubernetes_objects::rfc3339_seconds(created_at)),
        );
        if !self.labels.is_empty() {
            metadata.insert("labels".to_string(), ::serde_json::json!(self.labels));
        }
        if !self.annotations.is_empty() {
            metadata.insert(
                "annotations".to_string(),
                ::serde_json::json!(self.annotations),
            );
        }
        ::serde_json::Value::Object(metadata)
    }
}

/// The string-to-string map `field` of a manifest holds, or the `Status` the API
/// server refuses a member holding anything else with.
///
/// The ONE reader of such a member for this plane: `metadata.labels`,
/// `metadata.annotations` and a ConfigMap's `data` and `binaryData`
/// ([`super::kubernetes_config_maps`]) are all string-to-string maps, so one
/// reader keeps their wording and their behaviour in step. `field` is the path
/// the refusal names the member by, and `entries` the value the manifest carried
/// there — absent or null for a manifest that names none, which holds none.
pub(super) fn string_value_map(
    kind: &str,
    name: &str,
    field: &str,
    entries: Option<&::serde_json::Value>,
) -> Result<BTreeMap<String, String>, ApiRefusal> {
    let Some(entries) = entries.filter(|entries| !entries.is_null()) else {
        return Ok(BTreeMap::new());
    };
    let entries = entries.as_object().ok_or_else(|| {
        invalid_member(
            kind,
            name,
            field,
            "must be a map of string keys to string values",
        )
    })?;
    let mut values = BTreeMap::new();
    for (key, value) in entries {
        let value = value.as_str().ok_or_else(|| {
            invalid_member(kind, name, &format!("{field}[{key}]"), "must be a string")
        })?;
        values.insert(key.clone(), value.to_string());
    }
    Ok(values)
}

/// The `Status` the API server refuses a manifest whose `field` holds a value the
/// object cannot, in the wording apimachinery words an invalid member in:
/// `<kind> "<name>" is invalid: <field>: Invalid value: <detail>`.
///
/// The value itself is NOT quoted back: a member of a manifest read under this
/// address can be as large as the whole body, and a refusal is not the place to
/// echo it. `detail` names what the member has to be instead.
pub(super) fn invalid_member(kind: &str, name: &str, field: &str, detail: &str) -> ApiRefusal {
    ApiRefusal::invalid(format!(
        "{kind} \"{name}\" is invalid: {field}: Invalid value: {detail}"
    ))
}
