// SPDX-License-Identifier: CC0-1.0

//! The resource-based policy a resource of this service carries: the document
//! `PutResourcePolicy` attaches to one resource, which `GetResourcePolicy`
//! reports and `DeleteResourcePolicy` removes.
//!
//! A policy belongs to the resource its ARN names, so it is keyed by that
//! resource rather than by the ARN string: `<arn resource type>/<id>`, which is
//! exactly the resource part of the ARN the request addressed. A request reaches
//! the key through [`ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::resource_policy_target`],
//! which resolves an ARN only to a resource this service actually holds, so a
//! stored policy is never reported for a resource that was never created.
//!
//! ## What the ARN a request addresses resolves to
//!
//! The resource TYPE the ARN carries decides which of four answers a
//! resource-policy call gets ([`ResourcePolicyTarget`]), because the three
//! conditions a client branches on have three different remedies:
//!
//! * a type whose resources carry a policy plane ([`POLICY_CARRYING_TARGETS`]),
//!   resolving to a resource this service holds — the policy is stored, read or
//!   removed;
//! * such a type naming no resource this service holds — the modelled 404: create
//!   the resource, or address one that exists;
//! * another resource type OF THIS SERVICE ([`AGENTCORE_RESOURCE_TYPES`]) — the
//!   modelled `ValidationException`: that resource takes no resource policy, so
//!   the request is what has to change;
//! * an ARN naming no resource type of this service at all — the modelled
//!   `AccessDeniedException`: nothing of this account is addressed, so the call is
//!   denied rather than reported on.
//!
//! The policy map is bounded by the stores it keys into: one entry per
//! policy-carrying resource at most, and the entry of a resource that is gone is
//! dropped by [`ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::drop_orphaned_resource_policies`].

use super::*;

/// The ARN resource type of an AgentCore Runtime, as [`runtime_arn`] renders it.
pub const POLICY_TARGET_RUNTIME: &str = "runtime";

/// The ARN resource type of a Gateway, as [`gateway_arn`] renders it.
pub const POLICY_TARGET_GATEWAY: &str = "gateway";

/// Every ARN resource type whose resources carry a resource policy.
///
/// The operation's own documentation says the feature "is currently available
/// only for AgentCore Runtime and Gateway"
/// (`smithy_query(query="doc",shape="PutResourcePolicy")`), but a Browser and a
/// Code Interpreter are served exactly like a Gateway — accepted, reported back
/// and readable through `GetResourcePolicy` — so the recorded behaviour is what a
/// client sees and the documented pair is the narrower, stale statement. Each of
/// the four families is addressed under the ARN type its own `*Arn` pattern
/// publishes, and the two sandbox families carry two spellings each: the
/// customer-created one and the one this service provides
/// (`smithy_query(query="shape",shape="BrowserArn,CodeInterpreterArn")` —
/// `browser(-custom)?` and `code-interpreter(-custom)?`).
pub const POLICY_CARRYING_TARGETS: &[&str] = &[
    POLICY_TARGET_RUNTIME,
    POLICY_TARGET_GATEWAY,
    BROWSER_ARN_RESOURCE,
    SYSTEM_BROWSER_ARN_RESOURCE,
    CODE_INTERPRETER_ARN_RESOURCE,
    SYSTEM_CODE_INTERPRETER_ARN_RESOURCE,
];

/// Every ARN resource type a resource of THIS SERVICE is published under,
/// whether or not it carries a policy plane.
///
/// Source: each family's own `*Arn` pattern, as `crate::state`'s `*_arn`
/// readings of them render it —
/// `smithy_query(query="shape",shape="AgentRuntimeArn,GatewayArn,MemoryArn,RegistryArn,DatasetArn,HarnessArn,ConfigurationBundleArn,BrowserArn,BrowserProfileArn,CodeInterpreterArn,CapacityProviderArn,PaymentManagerArn,PolicyEngineArn,ConsentPortalArnType,EvaluatorArn,OnlineEvaluationConfigArn,ApiKeyCredentialProviderArnType,WorkloadIdentityArnType")`.
/// A credential provider and a workload identity are nested under the token
/// vault and the workload identity directory respectively, so the type their ARN
/// carries is the parent's.
///
/// What this list decides is only WHICH REFUSAL an out-of-scope ARN gets: a type
/// named here is a resource of this service that takes no policy (the request is
/// wrong), and a type absent from it addresses nothing of this account (the call
/// is denied).
pub const AGENTCORE_RESOURCE_TYPES: &[&str] = &[
    POLICY_TARGET_RUNTIME,
    POLICY_TARGET_GATEWAY,
    BROWSER_ARN_RESOURCE,
    SYSTEM_BROWSER_ARN_RESOURCE,
    CODE_INTERPRETER_ARN_RESOURCE,
    SYSTEM_CODE_INTERPRETER_ARN_RESOURCE,
    BROWSER_PROFILE_ARN_RESOURCE,
    CAPACITY_PROVIDER_ARN_RESOURCE,
    PAYMENT_MANAGER_ARN_RESOURCE,
    ARN_TYPE_CONFIGURATION_BUNDLE,
    RESOURCE_REGISTRY,
    "memory",
    "dataset",
    "harness",
    "evaluator",
    "online-evaluation-config",
    "policy-engine",
    "consent-portal",
    "token-vault",
    "workload-identity-directory",
];

/// The `resourceArn` member, as a refusal of the value it carried names it.
pub const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// What the ARN a resource-policy request addressed resolves to — the one
/// classification every resource-policy operation branches on, so none of them
/// can answer an ARN differently from the others (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourcePolicyTarget {
    /// The resource the ARN names, which this service holds and whose type
    /// carries a policy plane: the key its document is held under.
    Resolved(String),
    /// A policy-carrying resource type, naming no resource this service holds in
    /// the Region the request was signed for.
    Absent,
    /// A resource type of this service whose resources carry no resource policy.
    OutOfScope,
    /// An ARN naming no resource type of this service at all.
    Unrecognized,
}

/// The modelled not-found error for a resource policy this service does not
/// hold, in the words AWS answers a policy lookup that resolves no document
/// with ("Resource Policy doesn't exist for requested arn").
///
/// The sentence has no variable part: it names the ARN the request asked under
/// rather than quoting the caller's own value, so one answer covers every
/// resource of this service that carries no policy.
///
/// It is NOT the answer for an ARN that names no resource at all — that one is
/// [`resource_arn_not_found`]: the two conditions have different remedies (write
/// a policy, versus address a resource that exists), so they are worded apart.
pub fn resource_policy_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some("Resource Policy doesn't exist for requested arn".to_string()),
    }
}

/// The modelled not-found error for a `resourceArn` that names no resource a
/// policy of this service can attach to, in the words AWS answers it with
/// ("ResourceArn provided in the request does not exist").
///
/// Entirely fixed text: the ARN is the caller's own value and the remedy — name
/// a resource that exists — does not depend on it. Shared by every
/// resource-policy operation, so the same unresolvable ARN is reported in one
/// sentence whichever of them was asked.
pub fn resource_arn_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some("ResourceArn provided in the request does not exist".to_string()),
    }
}

/// The modelled validation error for a `resourceArn` that names a resource of
/// this service whose type carries no resource-policy plane.
///
/// It is the member's own refusal ([`field_validation_failed`], the shape this
/// service reports one refused member in), because the remedy is to change the
/// request rather than to create or address anything: no resource of that type
/// takes a policy, whichever one the ARN named. Shared by every resource-policy
/// operation, so none of them accepts a type another refuses.
pub fn resource_policy_unsupported_type() -> ValidationException {
    field_validation_failed(
        RESOURCE_ARN_MEMBER,
        "resource policies are not supported for this resource type",
    )
}

impl ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// What `resource_arn` resolves to for the resource-policy plane: the key a
    /// policy of it is held under, or which refusal the ARN earns (see the
    /// module docs and [`ResourcePolicyTarget`]).
    ///
    /// The ARN is resolved rather than parsed loosely: the resource part names
    /// the type and the id, and the whole ARN must be the one this service
    /// publishes for that resource in the region the request was signed for
    /// ([`runtime_arn`] / [`gateway_arn`] / [`browser_arn`] /
    /// [`code_interpreter_arn`] and the service-provided sandboxes' own
    /// readings), so a partition, account or region that is not this emulator's
    /// names no resource here.
    pub fn resource_policy_target(
        &self,
        region: &str,
        resource_arn: &str,
    ) -> ResourcePolicyTarget {
        let Some((_, resource)) = resource_arn.rsplit_once(':') else {
            return ResourcePolicyTarget::Unrecognized;
        };
        let Some((resource_type, resource_id)) = resource.split_once('/') else {
            return ResourcePolicyTarget::Unrecognized;
        };
        if !POLICY_CARRYING_TARGETS.contains(&resource_type) {
            return if AGENTCORE_RESOURCE_TYPES.contains(&resource_type) {
                ResourcePolicyTarget::OutOfScope
            } else {
                ResourcePolicyTarget::Unrecognized
            };
        }
        let published = match resource_type {
            POLICY_TARGET_RUNTIME if self.agent_runtimes.contains_key(resource_id) => {
                Some(runtime_arn(region, resource_id))
            }
            POLICY_TARGET_GATEWAY if self.gateways.contains_key(resource_id) => {
                Some(gateway_arn(region, resource_id))
            }
            BROWSER_ARN_RESOURCE if self.browsers.contains_key(resource_id) => {
                Some(browser_arn(region, resource_id))
            }
            // The sandboxes this service provides are held by nothing and
            // deleted by nothing, so the only id either type resolves is the
            // reserved one.
            SYSTEM_BROWSER_ARN_RESOURCE if resource_id == SYSTEM_BROWSER_ID => {
                Some(system_browser_arn(region))
            }
            CODE_INTERPRETER_ARN_RESOURCE if self.code_interpreters.contains_key(resource_id) => {
                Some(code_interpreter_arn(region, resource_id))
            }
            SYSTEM_CODE_INTERPRETER_ARN_RESOURCE if resource_id == SYSTEM_CODE_INTERPRETER_ID => {
                Some(system_code_interpreter_arn(region))
            }
            _ => None,
        };
        match published {
            Some(published) if published == resource_arn => {
                ResourcePolicyTarget::Resolved(resource.to_string())
            }
            _ => ResourcePolicyTarget::Absent,
        }
    }

    /// Attach `policy` to the resource `target` names, replacing whatever
    /// document that resource carried: one resource has one resource policy, so
    /// a second write is the new document rather than a second policy.
    pub fn attach_resource_policy(&mut self, target: String, policy: String) {
        self.resource_policies.insert(target, policy);
    }

    /// The document the resource `target` names carries, if it carries one.
    pub fn resource_policy(&self, target: &str) -> Option<&str> {
        self.resource_policies.get(target).map(String::as_str)
    }

    /// Remove the document the resource `target` names carries, answering it
    /// when there was one to remove.
    pub fn detach_resource_policy(&mut self, target: &str) -> Option<String> {
        self.resource_policies.remove(target)
    }

    /// Drop the policy of every resource this service no longer holds.
    ///
    /// A resource-based policy exists only as a property of its resource, so a
    /// resource that has been deleted — including one whose removal the clock
    /// made due — carries none: the removal was owed at the same instant as the
    /// resource's own, whether or not anybody called. Nothing a later caller can
    /// reach changes, because a policy is only ever read through
    /// [`Self::resource_policy_target`], which resolves no deleted resource.
    pub fn drop_orphaned_resource_policies(&mut self) {
        let orphaned: Vec<String> = self
            .resource_policies
            .keys()
            .filter(|target| !self.holds_resource_policy_target(target))
            .cloned()
            .collect();
        for target in orphaned {
            self.resource_policies.remove(&target);
        }
    }

    /// Whether this service still holds the resource `target` names.
    fn holds_resource_policy_target(&self, target: &str) -> bool {
        match target.split_once('/') {
            Some((POLICY_TARGET_RUNTIME, resource_id)) => {
                self.agent_runtimes.contains_key(resource_id)
            }
            Some((POLICY_TARGET_GATEWAY, resource_id)) => self.gateways.contains_key(resource_id),
            Some((BROWSER_ARN_RESOURCE, resource_id)) => self.browsers.contains_key(resource_id),
            Some((CODE_INTERPRETER_ARN_RESOURCE, resource_id)) => {
                self.code_interpreters.contains_key(resource_id)
            }
            // Nothing creates or deletes the sandboxes this service provides, so
            // a policy on one is never orphaned.
            Some((SYSTEM_BROWSER_ARN_RESOURCE, resource_id)) => resource_id == SYSTEM_BROWSER_ID,
            Some((SYSTEM_CODE_INTERPRETER_ARN_RESOURCE, resource_id)) => {
                resource_id == SYSTEM_CODE_INTERPRETER_ID
            }
            _ => false,
        }
    }
}
