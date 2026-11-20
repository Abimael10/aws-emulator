// SPDX-License-Identifier: CC0-1.0

//! The `spec` of a Pod one cluster's API server ADMITS: what the client
//! submitted, and what the API server requires of it before it stores anything.
//!
//! A Pod is the object a client creates through the published API server address
//! to run a workload, so the spec it submitted is what it reads back: the
//! container list that names the images, the pull secrets those pulls use, the
//! environment each container runs with, the volumes it mounts. The API server
//! answers a create with the object it STORED, which is the submitted spec with
//! the members it resolved itself — so nothing a client sent is dropped and
//! nothing it reads back was invented.
//!
//! `PodSpec.containers` is the one member the Kubernetes API requires of every
//! Pod: "list of containers belonging to the pod. Containers cannot currently be
//! added or removed. There must be at least one container in a Pod". A manifest
//! that names none, or names one with no name or no image, is refused with the
//! validation `Status` the API server refuses it with rather than stored as a Pod
//! with no workload — and so is one that names more containers than this server
//! holds a pod to ([`MAX_CONTAINERS_PER_POD`]), because every container of an
//! admitted pod is one more image its node's kubelet pulls
//! ([`super::kubernetes_image_pull`]).

use super::kubernetes_objects::ApiRefusal;

/// How many containers ONE pod may declare.
///
/// CHOSEN, not published: the Kubernetes API puts no limit on a pod's container
/// list, so the only bound on it would be the body ceiling the API server reads a
/// request under — which admits a pod declaring enough containers that the pull of
/// each one (an image reference resolved against a registry, over the policy
/// documents in force on the node's role) is work one caller chose for the cluster.
/// It is far above the handful a pod of this emulator is driven with, and a manifest
/// past it is refused with the validation `Status` apimachinery refuses a list past
/// its limit with rather than stored in part.
pub const MAX_CONTAINERS_PER_POD: usize = 64;

/// The `spec` the Pod `name` is ADMITTED with: the submitted one, validated,
/// carrying the service account the API server resolved for it.
///
/// `service_account_name` is the account the pod runs as, already resolved
/// against the namespace by the caller, so the stored spec names the account the
/// pod actually runs as rather than whatever the manifest left out. Both members
/// carry it: `serviceAccount` is the deprecated alias of `serviceAccountName`
/// and the API server keeps the two in step.
///
/// Nothing else of the submitted spec is rewritten — the containers read back
/// exactly as they were declared.
pub fn admitted_pod_spec(
    name: &str,
    manifest: &::serde_json::Value,
    service_account_name: &str,
) -> Result<::serde_json::Value, ApiRefusal> {
    let mut spec = match manifest.get("spec") {
        None | Some(::serde_json::Value::Null) => ::serde_json::json!({}),
        Some(spec @ ::serde_json::Value::Object(_)) => spec.clone(),
        Some(_) => {
            return Err(ApiRefusal::invalid(format!(
                "Pod \"{name}\" is invalid: spec: Invalid value: the pod's spec must be an \
                 object"
            )))
        }
    };
    validate_containers(name, spec.get("containers"))?;
    spec["serviceAccountName"] = ::serde_json::json!(service_account_name);
    spec["serviceAccount"] = ::serde_json::json!(service_account_name);
    Ok(spec)
}

/// Refuse a container list no Pod can run: one the manifest did not carry, an
/// empty one, an entry that is not an object, one naming no container or no
/// image, and one naming a container twice.
fn validate_containers(
    name: &str,
    submitted: Option<&::serde_json::Value>,
) -> Result<(), ApiRefusal> {
    let entries = submitted
        .and_then(::serde_json::Value::as_array)
        .filter(|entries| !entries.is_empty())
        .ok_or_else(|| required_value(name, "spec.containers"))?;
    if entries.len() > MAX_CONTAINERS_PER_POD {
        return Err(ApiRefusal::invalid(format!(
            "Pod \"{name}\" is invalid: spec.containers: Too many: {}: must have at most \
             {MAX_CONTAINERS_PER_POD} items",
            entries.len()
        )));
    }
    let mut named: ::std::collections::BTreeSet<&str> = ::std::collections::BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let container = entry
            .as_object()
            .ok_or_else(|| required_value(name, &format!("spec.containers[{index}].name")))?;
        let container_name = string_member(container, "name")
            .ok_or_else(|| required_value(name, &format!("spec.containers[{index}].name")))?;
        string_member(container, "image")
            .ok_or_else(|| required_value(name, &format!("spec.containers[{index}].image")))?;
        // "Each container in a pod must have a unique name": two containers of one
        // name name one container twice.
        if !named.insert(container_name) {
            return Err(ApiRefusal::invalid(format!(
                "Pod \"{name}\" is invalid: spec.containers[{index}].name: Duplicate value: \
                 \"{container_name}\""
            )));
        }
    }
    Ok(())
}

/// `member` of `document` as the non-empty string it has to be, and `None` for a
/// member that is absent, empty or not a string — all of which name nothing.
fn string_member<'document>(
    document: &'document ::serde_json::Map<String, ::serde_json::Value>,
    member: &str,
) -> Option<&'document str> {
    document
        .get(member)
        .and_then(::serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

/// The `Status` the API server refuses a Pod manifest that leaves a required
/// member out with, in the wording its validation words one in.
fn required_value(name: &str, member: &str) -> ApiRefusal {
    ApiRefusal::invalid(format!(
        "Pod \"{name}\" is invalid: {member}: Required value"
    ))
}
