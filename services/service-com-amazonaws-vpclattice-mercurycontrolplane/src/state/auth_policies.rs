// SPDX-License-Identifier: CC0-1.0

//! The auth policy of a service network or a service: what one holds, which identifiers
//! name one, the document the service accepts, and the state a read reports for it.
//!
//! An auth policy is NOT a resource of its own: it is a document attached to the service
//! network or service its `resourceIdentifier` names ("Creates or updates the auth
//! policy", `PutAuthPolicy`'s own documentation), so it lives in the one store keyed by
//! that resource's id and goes when the resource goes.
//!
//! Its reported STATE is derived, never stored: "The auth policy is only active when the
//! auth type is set to AWS_IAM. … If the Auth type is NONE, then, any auth policy that
//! you provide remains inactive" (the `state` member's own documentation), so every read
//! answers from the attached resource's auth type as it stands at read time — a later
//! `UpdateServiceNetwork` that flips that auth type flips what the same stored policy
//! reports, with nothing to migrate.

use std::time::SystemTime;

use super::{
    id_from_identifier, policy_target, ComAmazonawsVpclatticeMercurycontrolplaneState,
    ResourceKind, ServiceRuleViolation, POLICY_TARGET_KINDS,
};

/// The `AuthType` under which an attached auth policy decides requests
/// (`smithy_query(query="shape",shape="AuthType")`).
pub const AUTH_TYPE_AWS_IAM: &str = "AWS_IAM";

/// `AuthPolicyState` for a policy whose resource authenticates with IAM
/// (`smithy_query(query="shape",shape="AuthPolicyState")`).
pub const AUTH_POLICY_STATE_ACTIVE: &str = "Active";

/// `AuthPolicyState` for a stored policy that decides nothing, because its resource's
/// auth type is not [`AUTH_TYPE_AWS_IAM`].
pub const AUTH_POLICY_STATE_INACTIVE: &str = "Inactive";

/// The input member an auth-policy refusal names.
pub const AUTH_POLICY_MEMBER: &str = "policy";

/// How the service refuses a `policy` it cannot read as a JSON policy document.
///
/// An auth policy is an IAM policy document — a JSON object — and this is the sentence
/// the service answers one it cannot read with. SOURCE: recorded AWS behaviour for a
/// malformed auth policy (HTTP 400 `ValidationException`); the wording carries nothing
/// from the request, so the same sentence states every unreadable document.
pub const MALFORMED_AUTH_POLICY_MESSAGE: &str = "Not a valid policy";

/// The auth policy attached to one resource.
///
/// The document is kept VERBATIM: a read reports the policy the caller submitted, not a
/// re-serialization of it.
#[derive(Debug, Clone)]
pub struct AuthPolicy {
    /// The submitted document, byte for byte.
    pub policy: String,
    /// When the FIRST put attached a policy to this resource, which a later put that
    /// replaces the document keeps.
    pub created_at: SystemTime,
    /// When the latest put replaced the document.
    pub last_updated_at: SystemTime,
}

/// The kind and id an auth-policy `resourceIdentifier` names, or `None` when it names no
/// kind an auth policy can be attached to.
///
/// `ResourceIdentifier` admits BOTH forms — the bare id (`sn-…`, `svc-…`, `rcfg-…`) and
/// the resource's ARN — over the same three kinds a resource policy's ARN names
/// ([`POLICY_TARGET_KINDS`]), so the ARN form is resolved by exactly the function the
/// resource-policy operations use ([`policy_target`]) and the bare form by the shared id
/// reader ([`id_from_identifier`]).
pub fn auth_policy_target(resource_identifier: &str) -> Option<(ResourceKind, String)> {
    if resource_identifier.contains('/') {
        return policy_target(resource_identifier);
    }
    POLICY_TARGET_KINDS.into_iter().find_map(|kind| {
        id_from_identifier(resource_identifier, kind.id_prefix()).map(|id| (kind, id))
    })
}

/// Whether `policy` is a document this service stores, or the violation it is refused
/// with.
///
/// An auth policy is an IAM policy document, so a value that is not a JSON object is no
/// policy at all and nothing is written for it. The JSON is parsed by the crate this
/// service already depends on rather than inspected by hand, so exactly the documents a
/// JSON parser accepts are accepted here. WHITESPACE is not a fault: the documentation's
/// "must not contain newlines or blank lines" describes how a policy is written on the
/// wire, and an indented document is still a policy, so a document is judged by whether
/// it PARSES.
pub fn check_auth_policy_document(policy: &str) -> Result<(), ServiceRuleViolation> {
    let malformed = || ServiceRuleViolation {
        member: AUTH_POLICY_MEMBER.to_string(),
        message: MALFORMED_AUTH_POLICY_MESSAGE.to_string(),
    };
    let document = ::serde_json::from_str::<::serde_json::Value>(policy)
        .map_err(|_unparseable| malformed())?;
    if document.is_object() {
        Ok(())
    } else {
        Err(malformed())
    }
}

impl ComAmazonawsVpclatticeMercurycontrolplaneState {
    /// The `AuthType` of the resource `kind`/`id`, or `None` when this store holds no
    /// such resource.
    ///
    /// The one place a kind maps to where its auth type is kept, so the put and the read
    /// of an auth policy cannot disagree about what makes one active.
    pub fn auth_type_of(&self, kind: ResourceKind, id: &str) -> Option<&str> {
        match kind {
            ResourceKind::ServiceNetwork => self
                .service_networks
                .get(id)
                .map(|network| network.auth_type.as_str()),
            ResourceKind::Service => self
                .services
                .get(id)
                .map(|service| service.auth_type.as_str()),
            // No other kind models an `authType`, so no auth policy attached to one can
            // be active. A resource configuration is the only other kind an auth-policy
            // identifier names, and nothing holds one of those either.
            ResourceKind::ServiceAssociation
            | ResourceKind::VpcAssociation
            | ResourceKind::Listener
            | ResourceKind::Rule
            | ResourceKind::TargetGroup
            | ResourceKind::ResourceConfiguration => None,
        }
    }

    /// The `AuthPolicyState` a read reports for the policy attached to `kind`/`id`:
    /// active only while that resource authenticates with IAM.
    ///
    /// Derived from the stored auth type on every call, so it tracks a later update of
    /// that auth type; a pure function of stored state, so reading it changes nothing a
    /// later caller sees.
    pub fn auth_policy_state(&self, kind: ResourceKind, id: &str) -> &'static str {
        if self.auth_type_of(kind, id) == Some(AUTH_TYPE_AWS_IAM) {
            AUTH_POLICY_STATE_ACTIVE
        } else {
            AUTH_POLICY_STATE_INACTIVE
        }
    }

    /// Attach `policy` to the resource `id` at `now`, replacing whatever document that
    /// resource carried, and answer the record as stored.
    ///
    /// A replacement keeps the instant the first put attached a policy, so `createdAt`
    /// reports when the resource got its policy and `lastUpdatedAt` when the document
    /// last changed.
    pub fn put_auth_policy(&mut self, id: String, policy: String, now: SystemTime) -> &AuthPolicy {
        let record = self.auth_policies.entry(id).or_insert_with(|| AuthPolicy {
            policy: policy.clone(),
            created_at: now,
            last_updated_at: now,
        });
        record.policy = policy;
        record.last_updated_at = now;
        record
    }
}
