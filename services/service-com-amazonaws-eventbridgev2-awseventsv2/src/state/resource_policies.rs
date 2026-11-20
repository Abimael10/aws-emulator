// SPDX-License-Identifier: CC0-1.0

//! The resource-policy slice of this service's state: the named policies
//! `PutResourcePolicy` writes, `GetResourcePolicy` returns and
//! `ListResourcePolicies` summarises.
//!
//! An event bus is the only resource type this service attaches a policy to, so
//! a policy is stored ON its bus: the bus that is deleted takes its policies
//! with it, nothing beside the bus registry can grow, and no policy can outlive
//! the resource it is attached to. Nothing a policy response carries is computed
//! from the request alone — the document a read reports and the revision id a
//! write answers are both read back out of here.

use std::time::SystemTime;

use super::{bus_name_of_arn, EventBus};
use crate::ops::types::{
    AccessDeniedException, ConflictException, InvalidInputException, ResourceNotFoundException,
    ResourcePolicySummary,
};

/// The name of the customer-managed policy, which is also the policy an
/// operation that omits `PolicyName` addresses.
///
/// `PolicyName` documents its whole value list: "default" (the
/// customer-managed policy) and "AWS_RAM" (managed exclusively by AWS Resource
/// Access Manager).
pub const POLICY_NAME_DEFAULT: &str = "default";

/// The name of the policy AWS Resource Access Manager composes to reflect
/// resource shares; see [`POLICY_NAME_DEFAULT`] for where the pair comes from.
pub const POLICY_NAME_AWS_RAM: &str = "AWS_RAM";

/// Every name a policy of this service may carry: the whole of `PolicyName`'s
/// documented value list, in the order the model words it. A well-formed name
/// that is not one of these addresses no policy at all.
pub const POLICY_NAMES: &[&str] = &[POLICY_NAME_DEFAULT, POLICY_NAME_AWS_RAM];

/// The `ExpectedRevisionId` sentinel a policy that does not exist yet matches.
///
/// `PutResourcePolicy`'s `ExpectedRevisionId` documents "a policy that does not
/// exist yet matches only the sentinel "NO_POLICY" (create-only)".
pub const NO_POLICY_REVISION_ID: &str = "NO_POLICY";

/// The size quota of the "default" policy document, in bytes of UTF-8.
///
/// `PolicyDocument` documents "The "default" policy can be up to 20 KB (20,480
/// bytes of UTF-8) by default. This quota is adjustable in Service Quotas", and
/// a "default" policy past it is refused with `PolicyLengthExceededException`.
/// The 389,120-byte service maximum is the member's own modelled `@length`, so
/// the boundary walk refuses a document past THAT before any handler runs.
pub const DEFAULT_POLICY_MAX_BYTES: usize = 20_480;

/// One named policy attached to an event bus. Every member of
/// `GetResourcePolicyResponse` and of `ResourcePolicySummary` is a field here or
/// the name this policy is stored under.
#[derive(Debug, Clone)]
pub struct ResourcePolicy {
    /// The document as the write stored it, byte for byte — a read reports what
    /// was written rather than a re-rendering of it.
    pub document: String,
    /// The revision id minted by the write that stored this document.
    ///
    /// `PolicyRevisionId` documents "A new revision ID is generated on every
    /// successful write", so this changes with every write and is what a
    /// conditional write's `ExpectedRevisionId` is compared against.
    pub revision_id: String,
}

impl ResourcePolicy {
    /// This policy as `ListResourcePolicies` summarises it: the name it is
    /// stored under and its current revision id.
    pub fn summary(&self, policy_name: &str) -> ResourcePolicySummary {
        ResourcePolicySummary {
            policy_name: policy_name.to_string(),
            revision_id: self.revision_id.clone(),
        }
    }
}

impl EventBus {
    /// The named policy attached to this bus, or `None` when it holds none.
    pub fn resource_policy(&self, policy_name: &str) -> Option<&ResourcePolicy> {
        self.resource_policies.get(policy_name)
    }

    /// Attach `document` to this bus under `policy_name` and `revision_id`,
    /// replacing whatever that name held. The policy as stored is read back with
    /// [`Self::resource_policy`], so the response a write answers with comes out
    /// of the store rather than from beside it.
    pub fn store_resource_policy(
        &mut self,
        policy_name: &str,
        document: String,
        revision_id: String,
    ) {
        self.resource_policies.insert(
            policy_name.to_string(),
            ResourcePolicy {
                document,
                revision_id,
            },
        );
    }

    /// Detach the named policy from this bus, answering the policy as it was
    /// stored — `None` when the bus held none under that name, which is the
    /// delete that removes nothing. The revision a delete reports comes out of
    /// the policy handed back here rather than from beside the store, and the
    /// bus holds nothing under that name afterwards, so every later read of it
    /// reports the policy gone.
    pub fn remove_resource_policy(&mut self, policy_name: &str) -> Option<ResourcePolicy> {
        self.resource_policies.remove(policy_name)
    }
}

/// The policy an operation naming `requested` addresses: the name it gave, or
/// the customer-managed policy when it gave none.
///
/// ONE resolution, so a read, a write and a delete cannot disagree about what an
/// omitted `PolicyName` means. Every one of the four policy operations documents
/// it the same way: "Defaults to "default" … when omitted".
pub fn requested_policy_name(requested: Option<&str>) -> &str {
    requested.unwrap_or(POLICY_NAME_DEFAULT)
}

/// Whether `name` is one of the two policies this service attaches to a bus
/// ([`POLICY_NAMES`]).
pub fn is_named_policy(name: &str) -> bool {
    POLICY_NAMES.contains(&name)
}

/// The `InvalidInputException` every policy operation answers for a well-formed
/// `PolicyName` that is neither of the two named policies — the refusal
/// `PolicyName` words as "Any other value is rejected".
///
/// ONE construction, so no two operations word that refusal differently: a fixed
/// sentence listing the names the service admits, plus the name the request
/// submitted.
pub fn unknown_policy_name(policy_name: &str) -> InvalidInputException {
    InvalidInputException {
        message: Some(format!(
            "PolicyName {policy_name} is not valid; valid values are {}",
            POLICY_NAMES.join(" and ")
        )),
    }
}

/// The `AccessDeniedException` a WRITE of the other party's policy answers with.
///
/// The two writers of a bus's policies are exclusive in both directions — only
/// AWS Resource Access Manager writes "AWS_RAM" and only the bus owner writes
/// "default" — so a write that names the policy the caller does not own is
/// refused rather than applied. A request reaching this emulator arrives as the
/// bus owner, so the policy it may not write is the Resource Access Manager one.
/// ONE construction, shared by every operation that writes a policy.
pub fn policy_write_denied(policy_name: &str) -> AccessDeniedException {
    AccessDeniedException {
        message: Some(format!(
            "The {policy_name} resource policy is written exclusively by AWS Resource Access \
             Manager"
        )),
    }
}

/// The `ResourceNotFoundException` a policy-addressed operation answers with
/// when the bus exists but holds no policy under the requested name.
///
/// ONE construction, so no two operations word the same refusal differently: a
/// fixed sentence plus two variable parts, the policy name the request addressed
/// and the NAME of the bus it addressed it on — the same part of an
/// `EventBusArn` that [`super::event_bus_not_found`] reports, so a client reads
/// back the bus it asked about rather than a generated id segment.
pub fn resource_policy_not_found(
    policy_name: &str,
    event_bus_arn: &str,
) -> ResourceNotFoundException {
    let bus = bus_name_of_arn(event_bus_arn).unwrap_or(event_bus_arn);
    ResourceNotFoundException {
        message: Some(format!(
            "Resource policy not found: {policy_name} on event bus {bus}"
        )),
    }
}

/// Whether the policy a conditional write or delete found — `None` when the bus
/// holds none under that name — satisfies an `ExpectedRevisionId` of `expected`.
///
/// `PutResourcePolicy`'s `ExpectedRevisionId` documents both halves: the write
/// succeeds only if the named policy's current revision id matches the value,
/// and "a policy that does not exist yet matches only the sentinel "NO_POLICY"
/// (create-only)".
pub fn revision_matches(current: Option<&ResourcePolicy>, expected: &str) -> bool {
    match current {
        Some(policy) => policy.revision_id == expected,
        None => expected == NO_POLICY_REVISION_ID,
    }
}

/// The whole `Message` of the [`revision_conflict`] refusal: the FIXED text of
/// that error class, with no variable part at all.
///
/// Every fact it carries is a property of the error class rather than of one
/// request: which precondition failed, that the conflict can equally mean the
/// bus was recreated since the revision was read, what the caller must do
/// (re-read and re-evaluate, as `ConflictException` documents), and that a read
/// taken straight after a write can still report the earlier revision. The
/// precondition the caller sent and the policy it addressed are values the
/// caller already holds, so neither is interpolated into it.
pub const REVISION_CONFLICT_MESSAGE: &str = "The resource policy revision does not match \
     ExpectedRevisionId, or the event bus was recreated since that revision was read. Re-read \
     the policy and re-evaluate before retrying. A recent policy change can take up to a minute \
     to be reflected on all endpoints, so a read taken immediately after one may still return \
     the earlier revision.";

/// The `ConflictException` a conditional policy write or delete answers when the
/// `ExpectedRevisionId` it carried does not hold.
///
/// ONE construction, so no two operations word that refusal differently, and it
/// takes nothing of the request: the message is wholly fixed
/// ([`REVISION_CONFLICT_MESSAGE`]). The policy's CURRENT revision id is not part
/// of it either — `ConflictException` documents that "a successful attempt
/// stores a revision ID the caller never saw, so the revision alone cannot tell
/// the two apart", and tells the caller to compare the resource's contents
/// instead.
pub fn revision_conflict() -> ConflictException {
    ConflictException {
        message: Some(REVISION_CONFLICT_MESSAGE.to_string()),
    }
}

/// The `InvalidInputException` a DELETE answers when its `ExpectedRevisionId`
/// carries the [`NO_POLICY_REVISION_ID`] sentinel.
///
/// `DeleteResourcePolicy`'s `ExpectedRevisionId` documents that "The "NO_POLICY"
/// sentinel is not valid here": the sentinel stands for a policy that does not
/// exist yet, and a delete's precondition names a revision that must exist, so
/// it is refused as the bad parameter value it is rather than compared against
/// the store. The message is wholly fixed — the value the caller sent is the
/// sentinel itself, which the fixed text already names.
pub fn no_policy_sentinel_rejected() -> InvalidInputException {
    InvalidInputException {
        message: Some(format!(
            "ExpectedRevisionId {NO_POLICY_REVISION_ID} is not valid when deleting a resource \
             policy; it matches only a policy that does not exist yet"
        )),
    }
}

/// The revision id for `seed`: this service's one UUID rendering of a minted
/// identifier ([`super::new_uuid_id`]).
///
/// `PolicyRevisionId` is an opaque string of 1..256 characters that a client
/// only ever compares or echoes, and a UUID is the form this service renders its
/// other opaque server-assigned identifiers in, so the rendering is the shared
/// one rather than a second copy of it. `seed` comes from
/// [`super::ComAmazonawsEventbridgev2Awseventsv2State::next_id_seed`], which
/// hands out each value once, and the rendering is injective in it, so no two
/// writes of one store can mint the same revision — which is what makes a
/// conditional write see the writer it lost to.
pub fn new_policy_revision_id(seed: u64, now: SystemTime) -> String {
    super::new_uuid_id(seed, now)
}
