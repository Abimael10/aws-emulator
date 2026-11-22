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
//!
//! `PodSpec.restartPolicy` is the other member the API server decides here: it is
//! defaulted the way Kubernetes defaults it, so a client reads back the policy its
//! pod was admitted under, and refused when it names a policy Kubernetes does not
//! have — it is the member that says whether a pod's containers are started again
//! once they exit, so a value nothing defines is not stored.

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

/// How many TOLERATIONS one pod may declare.
///
/// CHOSEN, not published: the Kubernetes API puts no limit on a pod's toleration
/// list, and every toleration of an admitted pod is matched against every taint of
/// every node the cluster could place it on
/// ([`super::kubernetes_nodes::ClusterNode::admits`]) — work one caller would
/// otherwise choose for the cluster. It is far above the handful a pod of this
/// emulator is driven with, and a manifest past it is refused with the validation
/// `Status` apimachinery refuses a list past its limit with rather than stored in
/// part.
pub const MAX_TOLERATIONS_PER_POD: usize = 64;

/// The `PodSpec.restartPolicy` values Kubernetes accepts: "restart policy for all
/// containers within the pod. One of Always, OnFailure, Never. ... Default to
/// Always." The policy is what says whether a pod's containers are started again
/// once they exit, so a manifest naming a value outside this set is refused rather
/// than stored as a pod whose lifecycle nothing defines.
pub const POD_RESTART_POLICY_ALWAYS: &str = "Always";
/// See [`POD_RESTART_POLICY_ALWAYS`].
pub const POD_RESTART_POLICY_ON_FAILURE: &str = "OnFailure";
/// See [`POD_RESTART_POLICY_ALWAYS`].
pub const POD_RESTART_POLICY_NEVER: &str = "Never";
/// The three of them, in the order the API server's own validation lists them.
pub const POD_RESTART_POLICIES: [&str; 3] = [
    POD_RESTART_POLICY_ALWAYS,
    POD_RESTART_POLICY_ON_FAILURE,
    POD_RESTART_POLICY_NEVER,
];

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
    validate_tolerations(name, spec.get("tolerations"))?;
    let restart_policy = admitted_restart_policy(name, &spec)?;
    spec["restartPolicy"] = ::serde_json::json!(restart_policy);
    spec["serviceAccountName"] = ::serde_json::json!(service_account_name);
    spec["serviceAccount"] = ::serde_json::json!(service_account_name);
    Ok(spec)
}

/// The `restartPolicy` the pod is admitted with: the one the manifest named, or
/// the Kubernetes default for a manifest that named none ("default to Always").
///
/// A value outside [`POD_RESTART_POLICIES`] is refused with the validation
/// `Status` apimachinery refuses an unsupported enumerated value with, listing the
/// values it does support — the policy decides the pod's whole lifecycle, so a
/// value nothing defines is not stored.
fn admitted_restart_policy(
    name: &str,
    spec: &::serde_json::Value,
) -> Result<&'static str, ApiRefusal> {
    let Some(named) = spec
        .get("restartPolicy")
        .and_then(::serde_json::Value::as_str)
        .filter(|policy| !policy.is_empty())
    else {
        return Ok(POD_RESTART_POLICY_ALWAYS);
    };
    POD_RESTART_POLICIES
        .into_iter()
        .find(|policy| *policy == named)
        .ok_or_else(|| {
            let supported = POD_RESTART_POLICIES
                .map(|policy| format!("\"{policy}\""))
                .join(", ");
            ApiRefusal::invalid(format!(
                "Pod \"{name}\" is invalid: spec.restartPolicy: Unsupported value: \"{named}\": \
                 supported values: {supported}"
            ))
        })
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

/// Refuse a toleration list no pod can be scheduled against: one that is not a
/// list at all, and one past [`MAX_TOLERATIONS_PER_POD`] — every entry of it is
/// matched against the taints of every node the cluster could place the pod on, so
/// the list is held to a length before the pod is stored.
fn validate_tolerations(
    name: &str,
    submitted: Option<&::serde_json::Value>,
) -> Result<(), ApiRefusal> {
    let Some(submitted) = submitted.filter(|submitted| !submitted.is_null()) else {
        return Ok(());
    };
    let entries = submitted.as_array().ok_or_else(|| {
        ApiRefusal::invalid(format!(
            "Pod \"{name}\" is invalid: spec.tolerations: Invalid value: the pod's tolerations \
             must be a list"
        ))
    })?;
    if entries.len() > MAX_TOLERATIONS_PER_POD {
        return Err(ApiRefusal::invalid(format!(
            "Pod \"{name}\" is invalid: spec.tolerations: Too many: {}: must have at most \
             {MAX_TOLERATIONS_PER_POD} items",
            entries.len()
        )));
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
