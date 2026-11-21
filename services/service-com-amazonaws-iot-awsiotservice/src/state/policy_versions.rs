// SPDX-License-Identifier: CC0-1.0

//! The VERSIONS of an IoT policy: how a version is added, how many a policy holds,
//! and what a reader of them reports.
//!
//! A policy is a name plus a set of numbered document versions and a pointer to the
//! one in effect ([`super::RegisteredPolicy`]). This is the half of that record the
//! version operations write and read: `CreatePolicyVersion` adds one and may re-point
//! the pointer, `ListPolicyVersions` reports them, `GetPolicyVersion` reports one,
//! `SetDefaultPolicyVersion` re-points the pointer and `DeletePolicyVersion` removes
//! one that is not in effect.
//!
//! How MANY versions a policy holds is CAPPED, which is what makes
//! `CreatePolicyVersion`'s modelled `VersionsLimitExceededException` reachable: past
//! the cap nothing is written and the create is refused, so a policy's versions never
//! outnumber [`MAX_POLICY_VERSIONS`] however many callers add them at once.

use std::time::SystemTime;

use crate::ops::types::PolicyVersion as ModeledPolicyVersion;

/// How many versions one IoT policy holds.
///
/// AWS publishes the quota as five versions of a policy, which is the cap
/// `CreatePolicyVersion`'s modelled `VersionsLimitExceededException` ("The number of
/// policy versions exceeds the limit", HTTP 409) is refused past
/// (`smithy_query(query="operation",operation="CreatePolicyVersion")`): a policy at
/// the cap takes no further version until one is deleted.
pub const MAX_POLICY_VERSIONS: usize = 5;

/// How this service refuses a policy that already holds [`MAX_POLICY_VERSIONS`]
/// versions.
///
/// The fixed text is the class's, and the variable parts are the policy the request
/// named and the cap it is at.
pub fn policy_versions_limit_message(policy_name: &str) -> String {
    format!(
        "The policy {policy_name} already has the maximum number of versions \
         ({MAX_POLICY_VERSIONS})"
    )
}

/// How an operation that addresses ONE version of a policy refuses a `policyVersionId`
/// the policy does not hold.
///
/// The POLICY VERSION family states that miss in the parenthesised form this crate's other
/// refusal about one version already uses
/// ([`default_policy_version_deletion_message`]): fixed text, then the policy and the
/// version the request named as `name=` and `versionId=`. One sentence covers a read of a
/// version, a delete of one and a re-pointing at one, so the three cannot word one miss
/// three ways.
pub fn missing_policy_version_message(policy_name: &str, policy_version_id: &str) -> String {
    format!("Policy version does not exist (name={policy_name}, versionId={policy_version_id})")
}

/// How a reader of the policy's DEFAULT version refuses a name the registry does not
/// hold.
///
/// `GetPolicy` reports "the policy document of the default version"
/// (`smithy_query(query="doc",shape="GetPolicy")`), so it resolves the requested name
/// through that version — and AWS words its miss after the version it could not find
/// rather than after the name it was given, which is why this sentence is entirely fixed
/// text with no variable part, unlike [`super::missing_policy_message`], which every
/// operation addressing the policy RECORD states a miss with.
pub fn missing_policy_default_version_message() -> String {
    "Policy default version not found".to_string()
}

/// How a delete refuses the version that is currently the policy's DEFAULT one.
///
/// `DeletePolicyVersion` documents the rule: "You cannot delete the default version of
/// a policy using this action. To delete the default version of a policy, use
/// DeletePolicy" (`smithy_query(query="doc",shape="DeletePolicyVersion")`). The fixed
/// text is the class's, and the variable parts are the policy and the version the
/// request addressed, named the `name=` way this crate's other policy delete conflicts
/// name them ([`super::policy_deletion::non_default_versions_deletion_message`]).
pub fn default_policy_version_deletion_message(
    policy_name: &str,
    policy_version_id: &str,
) -> String {
    format!("Cannot delete a default policy version (name={policy_name}, versionId={policy_version_id})")
}

/// Why a version of a policy was NOT deleted.
///
/// Each variant names the modelled error class the refusal answers with
/// (`smithy_query(query="operation",operation="DeletePolicyVersion")`), so the handler
/// chooses the status from the state rather than restating the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyVersionDeletionRefusal {
    /// A policy, or a version of it, the account does not hold:
    /// `ResourceNotFoundException` (HTTP 404). Carries the message.
    NotFound(String),
    /// The version the request named is the one in effect: `DeleteConflictException`
    /// (HTTP 409). Carries the message.
    DefaultVersion(String),
}

impl PolicyVersionDeletionRefusal {
    /// The message the refusal states, whichever class it names.
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(message) | Self::DefaultVersion(message) => message,
        }
    }
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// Delete the version `policy_version_id` of the policy `policy_name`, or answer the
    /// reason this account's state held it back.
    ///
    /// The guard is read under the same lock as the write, so a version that becomes the
    /// default between the check and the removal cannot be deleted. The version in
    /// effect is NOT deletable — it goes with the policy, through `DeletePolicy` — and
    /// every other version is removed from the record the policy's versions live on, so
    /// every later reader of them stops reporting it while the policy itself is left
    /// standing.
    pub fn delete_policy_version(
        &mut self,
        policy_name: &str,
        policy_version_id: &str,
        now: SystemTime,
    ) -> Result<(), PolicyVersionDeletionRefusal> {
        let Some(policy) = self.policies.get_mut(policy_name) else {
            return Err(PolicyVersionDeletionRefusal::NotFound(
                super::missing_policy_message(policy_name),
            ));
        };
        let Some(version_id) = policy_version_id.parse::<u32>().ok().filter(|version_id| {
            policy.versions.contains_key(version_id)
        }) else {
            return Err(PolicyVersionDeletionRefusal::NotFound(
                missing_policy_version_message(policy_name, policy_version_id),
            ));
        };
        if version_id == policy.default_version_id {
            return Err(PolicyVersionDeletionRefusal::DefaultVersion(
                default_policy_version_deletion_message(policy_name, policy_version_id),
            ));
        }
        policy.versions.remove(&version_id);
        policy.last_modified_date = now;
        policy.mint_generation_id();
        Ok(())
    }
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// Point the policy `policy_name` at `policy_version_id` as the version in EFFECT, or
    /// answer the message stating what this account's state holds nothing under.
    ///
    /// "Sets the specified version of the specified policy as the policy's default
    /// (operative) version" (`smithy_query(query="doc",shape="SetDefaultPolicyVersion")`),
    /// so the pointer every reader of the policy resolves through — `GetPolicy`'s
    /// `defaultVersionId` and the document it hands back, `GetPolicyVersion`'s and
    /// `ListPolicyVersions`' `isDefaultVersion`, and the version `DeletePolicyVersion`
    /// holds back — moves with this one write. A policy, or a version of it, the account
    /// does not hold is the modelled `ResourceNotFoundException` (HTTP 404) stated in the
    /// wording that reader of the policy or of one of its versions refuses a miss with, and
    /// nothing is written.
    ///
    /// Re-pointing a policy at the version already in effect writes the pointer it already
    /// carries, so the policy's generation moves the way any other write of it does: the
    /// call is a write, not a read.
    pub fn set_default_policy_version(
        &mut self,
        policy_name: &str,
        policy_version_id: &str,
        now: SystemTime,
    ) -> Result<(), String> {
        let Some(policy) = self.policies.get_mut(policy_name) else {
            return Err(super::missing_policy_message(policy_name));
        };
        let Some(version_id) = policy_version_id
            .parse::<u32>()
            .ok()
            .filter(|version_id| policy.versions.contains_key(version_id))
        else {
            return Err(missing_policy_version_message(policy_name, policy_version_id));
        };
        policy.default_version_id = version_id;
        policy.last_modified_date = now;
        policy.mint_generation_id();
        Ok(())
    }
}

impl super::RegisteredPolicy {
    /// Add `document` as a new version, making it the default when the request asked
    /// for that, and answer the id it was given — or `None` when the policy is
    /// already at [`MAX_POLICY_VERSIONS`], in which case nothing is written.
    ///
    /// The id is one past the highest the policy has ever held, so an id is never
    /// reused by a later version: a caller that deleted version 2 and adds another
    /// gets 3, not 2. The cap is read under the same lock as the write, so two
    /// concurrent creates cannot both find room for the same slot.
    pub fn add_version(
        &mut self,
        document: String,
        set_as_default: bool,
        now: SystemTime,
    ) -> Option<u32> {
        if self.versions.len() >= MAX_POLICY_VERSIONS {
            return None;
        }
        let version_id = self
            .versions
            .keys()
            .next_back()
            .map_or(super::FIRST_POLICY_VERSION_ID, |highest| highest + 1);
        self.versions.insert(
            version_id,
            super::PolicyVersion {
                document,
                created_at: now,
            },
        );
        if set_as_default {
            self.default_version_id = version_id;
        }
        self.last_modified_date = now;
        self.mint_generation_id();
        Some(version_id)
    }

    /// The document of the version `policy_version_id`, or `None` for an id the
    /// policy does not hold.
    ///
    /// The id is the modelled `PolicyVersionId` (`^[0-9]+$`), so anything that is not
    /// a number names no version of any policy.
    pub fn version(&self, policy_version_id: &str) -> Option<&super::PolicyVersion> {
        let version_id = policy_version_id.parse::<u32>().ok()?;
        self.versions.get(&version_id)
    }

    /// Whether `policy_version_id` names the version in effect.
    pub fn is_default_version(&self, policy_version_id: &str) -> bool {
        policy_version_id
            .parse::<u32>()
            .is_ok_and(|version_id| version_id == self.default_version_id)
    }

    /// Every version as `ListPolicyVersions` reports them, NEWEST-first: the version a
    /// caller just created, or is about to point the policy at, is the one it reads at the
    /// head of the list, and the oldest version it still holds is at the tail.
    ///
    /// The records are kept oldest-first (the ids ascend, [`RegisteredPolicy::versions`]),
    /// so the reporting order is that order walked backwards; a version the policy no
    /// longer holds is in neither.
    pub fn version_summaries(&self) -> Vec<ModeledPolicyVersion> {
        self.versions
            .iter()
            .rev()
            .map(|(version_id, version)| ModeledPolicyVersion {
                version_id: Some(version_id.to_string()),
                is_default_version: *version_id == self.default_version_id,
                create_date: Some(::aws_smithy_types::DateTime::from(version.created_at)),
            })
            .collect()
    }
}
