// SPDX-License-Identifier: CC0-1.0

//! The rules in force for ONE identity in ONE namespace: everything the cluster
//! lets it do there.
//!
//! The authorizer answers one question at a time — may this identity do THIS
//! ([`admits`]) — and a client that wants to know what it may do at all asks for
//! the rules themselves. Both read the SAME two sources of rules (the bindings the
//! cluster holds, and the published permissions its groups and its access policies
//! carry) and resolve a binding's `roleRef` through the same
//! [`ClusterObjects::role_ref_rules`], so the rules a client is told it has cannot
//! disagree with the decision taken on a request of its own.
//!
//! A rule is collected BORROWED ([`RoleRules`]) rather than copied: the published
//! tables are `&'static` and a stored role's rules live in the cluster's own store,
//! so resolving a `roleRef` allocates nothing and one decision still costs the
//! bounded walk [`super::stored_objects`] describes.

use super::*;

/// One rule in force, as a review reports it: the members a Kubernetes
/// `PolicyRule` carries, each already in the form the answer renders.
///
/// Collected from both sources of rules — a role a client created
/// ([`PolicyRule`]) and a published table ([`PublishedRule`]) — so a review
/// reports the permissions of either in one shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRule {
    pub verbs: Vec<String>,
    pub api_groups: Vec<String>,
    pub resources: Vec<String>,
    pub resource_names: Vec<String>,
    pub non_resource_urls: Vec<String>,
}

impl EffectiveRule {
    /// Whether this rule is about RESOURCES, which is what a review reports under
    /// its resource rules.
    #[must_use]
    pub fn is_resource_rule(&self) -> bool {
        !self.resources.is_empty()
    }

    /// Whether this rule is about PATHS of the server's own, which is what a
    /// review reports under its non-resource rules. A rule may be both, and one
    /// that is neither constrains nothing and is reported as neither.
    #[must_use]
    pub fn is_non_resource_rule(&self) -> bool {
        !self.non_resource_urls.is_empty()
    }

    /// The rule a stored role carries.
    fn of_policy_rule(rule: &PolicyRule) -> Self {
        Self {
            verbs: rule.verbs.clone(),
            api_groups: rule.api_groups.clone(),
            resources: rule.resources.clone(),
            resource_names: rule.resource_names.clone(),
            non_resource_urls: rule.non_resource_urls.clone(),
        }
    }

    /// The rule a published table carries. A published rule names no
    /// `resourceNames`, so it constrains no single object.
    fn of_published_rule(rule: &PublishedRule) -> Self {
        let owned = |values: &[&str]| values.iter().map(|value| (*value).to_string()).collect();
        Self {
            verbs: owned(rule.verbs),
            api_groups: owned(rule.api_groups),
            resources: owned(rule.resources),
            resource_names: Vec::new(),
            non_resource_urls: owned(rule.non_resource_urls),
        }
    }
}

/// The rules of ONE role, wherever that role lives: the objects this cluster
/// holds, or a table Kubernetes publishes for every cluster.
///
/// `None` is a `roleRef` that names no role at all — a dangling reference, which
/// RBAC reads as granting nothing.
pub(super) enum RoleRules<'objects> {
    Stored(&'objects [PolicyRule]),
    Published(&'static [PublishedRule]),
    None,
}

impl RoleRules<'_> {
    /// Whether any rule of this role admits `attributes`.
    #[must_use]
    pub(super) fn admits(&self, attributes: &RequestAttributes) -> bool {
        match self {
            Self::Stored(rules) => rules.iter().any(|rule| rule.admits(attributes)),
            Self::Published(rules) => rules.iter().any(|rule| rule.admits(attributes)),
            Self::None => false,
        }
    }

    /// Add every rule of this role to `into`, in the order the role carries them.
    pub(super) fn collect_into(&self, into: &mut Vec<EffectiveRule>) {
        match self {
            Self::Stored(rules) => into.extend(rules.iter().map(EffectiveRule::of_policy_rule)),
            Self::Published(rules) => {
                into.extend(rules.iter().map(EffectiveRule::of_published_rule));
            }
            Self::None => {}
        }
    }
}

/// The sentence an authorizer that cannot resolve a user's rules reports, which the
/// listing carries as its `evaluationError`.
///
/// "EvaluationError can appear in combination with Rules. It indicates an error
/// occurred during rule evaluation" (`SubjectRulesReviewStatus$evaluationError`),
/// and the error a WEBHOOK authorizer answers a request for a user's rules with is
/// this one: it is asked one request at a time and has no rules to enumerate. It is
/// the whole fixed sentence — it names nothing of the request or of the cluster.
pub const WEBHOOK_RULE_RESOLUTION_ERROR: &str =
    "webhook authorizer does not support user rule resolution";

/// The rules in force for one identity in one namespace, and whether they are all
/// of them.
///
/// "Incomplete is true when the rules returned by this call are incomplete"
/// (`SubjectRulesReviewStatus$incomplete`): a Kubernetes API server can enumerate
/// the rules of its RBAC authorizer, and cannot enumerate the rules of an
/// authorizer that is asked one request at a time — a WEBHOOK, which is what Amazon
/// EKS's access entries are served by. So a listing taken while such an authorizer
/// is in the cluster's chain is incomplete, and reports the error that authorizer
/// answered the request for its rules with.
#[derive(Debug, Clone)]
pub struct RulesListing {
    pub rules: Vec<EffectiveRule>,
    pub incomplete: bool,
    /// The error reported beside an incomplete listing, and `None` for a listing
    /// every authorizer of the chain answered.
    pub evaluation_error: Option<&'static str>,
}

/// Every rule in force for `identity` in `namespace` that this cluster can
/// ENUMERATE, in the order the sources of rules are read: the roles Kubernetes
/// publishes for the groups it is in, the roles the cluster's own bindings bind it
/// to, and — only where the access-entry authorizer is not the one deciding them —
/// the access policies its access entry associates.
///
/// `access_entry_authorizer` is whether the cluster's chain includes the authorizer
/// Amazon EKS serves its access entries by
/// ([`ClusterRecord::serves_access_entries`]). Where it does, that authorizer
/// decides what the associated access policies grant and its rules cannot be
/// enumerated, so they are left out of the listing and the listing reports itself
/// incomplete; where it does not, the policies an entry carries are decided by this
/// cluster's own reading of them ([`admits`]) and are therefore enumerable like any
/// other rule — which is what keeps a listing in agreement with the decision the
/// same state takes on a request.
///
/// The namespace decides which of them apply: an access policy associated to
/// namespaces reaches only those ([`AccessPolicyGrant::applies_in`]), a
/// `ClusterRoleBinding` reaches every namespace, and a `RoleBinding` only its
/// own. Nothing is collected for a namespace the cluster does not hold beyond the
/// cluster-wide rules, which is what the API server reports for one.
#[must_use]
pub fn effective_rules(
    objects: &ClusterObjects,
    identity: &SubjectIdentity,
    namespace: &str,
    access_entry_authorizer: bool,
) -> RulesListing {
    let mut rules = Vec::new();
    if !access_entry_authorizer {
        for grant in &identity.grants {
            if !grant.applies_in(namespace) {
                continue;
            }
            if let Some(published) = access_policy_rules(&grant.policy_name) {
                RoleRules::Published(published).collect_into(&mut rules);
            }
        }
    }
    for group in &identity.groups {
        if let Some(published) = published_group_rules(group) {
            RoleRules::Published(published).collect_into(&mut rules);
        }
    }
    objects.bound_rules(identity, namespace, &mut rules);
    RulesListing {
        rules,
        incomplete: access_entry_authorizer,
        evaluation_error: access_entry_authorizer.then_some(WEBHOOK_RULE_RESOLUTION_ERROR),
    }
}
