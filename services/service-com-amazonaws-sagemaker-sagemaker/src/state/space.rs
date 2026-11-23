// SPDX-License-Identifier: CC0-1.0

//! The STUDIO SPACE slice of this service's state: the space `CreateSpace` writes,
//! `DescribeSpace` replays and `DeleteSpace` tears down, with its ARN kind, its store key,
//! its statuses, the clock spans it moves on, its store bound and the sentences its
//! refusals are worded with.
//!
//! A space is a MEMBER of a Studio domain, exactly as a user profile is ("Creates a private
//! space or a space used for real time collaboration in a domain"), so this slice mirrors
//! [`super::user_profile`]: the domain is half of a space's identity, the domain has to
//! admit members before one can be created, and a space of a domain the clock has torn down
//! resolves to nothing.
//!
//! Its own file, like the other resource slices here, so the parent module stays inside
//! the project's file-length limit. Every name is re-exported from [`super`], so a
//! handler keeps naming `crate::state::…`.

use super::*;
use crate::ops::types::{OwnershipSettings, SpaceSettings, SpaceSharingSettings};

/// The ARN resource kind a space carries, i.e. the `<kind>` in [`resource_arn`]. Fixed by
/// `SpaceArn`'s modelled pattern (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:space/`),
/// and named once here so the create and every reader mint and match the same ARN.
pub const SPACE_ARN_KIND: &str = "space";

/// See [`USER_PROFILE_QUOTA`]. Bounds the space store the same way, and is what
/// `CreateSpace` answers `ResourceLimitExceeded` past, so a caller creating space after
/// space cannot grow the store without end.
pub const SPACE_QUOTA: usize = 1_000;

/// `SpaceStatus`'s members a space of this emulator moves through. The modelled enum also
/// carries the update and failure states, which no implemented operation reaches yet
/// (`smithy_query(query="shape", shape="SpaceStatus")`).
pub const SPACE_STATUS_PENDING: &str = "Pending";
/// See [`SPACE_STATUS_PENDING`].
pub const SPACE_STATUS_IN_SERVICE: &str = "InService";
/// See [`SPACE_STATUS_PENDING`]. What a space reports from the `DeleteSpace` that asked for
/// its removal until the clock completes it ([`Space::begin_deletion`]).
pub const SPACE_STATUS_DELETING: &str = "Deleting";

/// How long after `CreateSpace` the clock owes the `Pending` -> `InService` transition.
///
/// The same compressed, never-zero span the domain's own provisioning and its user
/// profiles' take ([`USER_PROFILE_IN_SERVICE_AFTER`]): a caller that describes the space
/// straight after creating it still observes `Pending`, and one that waits observes
/// `InService`. The due instant is stored rather than slept on ([`Space::settle_status`]).
pub const SPACE_IN_SERVICE_AFTER: Duration = USER_PROFILE_IN_SERVICE_AFTER;

/// How long after `DeleteSpace` the clock owes the `Deleting` -> gone transition of a space.
///
/// The same compressed, never-zero span a user profile's teardown takes
/// ([`USER_PROFILE_RELEASED_AFTER`]): a caller that describes the space straight after
/// asking for its deletion still observes `Deleting`, and one that polls observes it gone.
/// The due instant is stored rather than slept on ([`Space::is_released`]).
pub const SPACE_RELEASED_AFTER: Duration = USER_PROFILE_RELEASED_AFTER;

/// The entity label the Studio refusals name a space by, i.e. the subject of
/// [`crate::state::domain_not_in_service_refusal`] and of
/// [`crate::state::studio_member_already_exists`] when the resource being created is a
/// space.
pub const SPACE_ENTITY: &str = "Space";

/// The store key a `(DomainId, SpaceName)` pair maps to.
///
/// A space is created "in a domain", so the domain is part of its identity: the same name
/// in two domains is two spaces. Unlike `UserProfileName`, whose doc makes it "not case
/// sensitive", `SpaceName`'s model doc says nothing about case, so the name is the key
/// VERBATIM and two spellings are two spaces.
pub fn space_key(domain_id: &str, space_name: &str) -> (String, String) {
    (domain_id.to_string(), space_name.to_string())
}

/// Where the space `space_name` of the domain whose Studio address is `domain_url` is
/// served, i.e. the `Url` `DescribeSpace` reports.
///
/// Derived from the domain's own Studio host ([`studio_domain_url`]) and the space's name,
/// so it is stable across reads and distinct per space — the space is reached THROUGH its
/// domain, and the member's doc has the caller append a redirect parameter to whatever this
/// answers (`smithy_query(query="doc", shape="DescribeSpaceResponse", member="Url")`).
pub fn studio_space_url(domain_url: &str, space_name: &str) -> String {
    format!("{domain_url}/space/{space_name}")
}

/// How the operations whose SUBJECT is the space itself state a `(DomainId, SpaceName)` pair
/// this account holds no space under: the entity, then the ARN the requested pair resolves
/// to in the Region the request signed for IN SQUARE BRACKETS, then `does not exist.` — the
/// sentence the Studio family's readers word this fact with, as
/// [`crate::state::user_profile_arn_not_found`] does for a user profile and
/// [`crate::state::domain_not_found`] for the domain both live in.
///
/// `DescribeSpace` and `DeleteSpace` address the SPACE, so they name it by the ARN their
/// request resolves to; an operation whose subject is something else and that merely
/// resolves a space as a member names the value the caller passed, the way
/// [`crate::state::domain_id_not_found`] does for the domain a member-create names.
pub fn space_arn_not_found(region: &str, domain_id: &str, space_name: &str) -> String {
    format!(
        "Space [{}] does not exist.",
        resource_arn(
            region,
            SPACE_ARN_KIND,
            &format!("{domain_id}/{space_name}")
        )
    )
}

/// A Studio space created by `CreateSpace`, as `DescribeSpace` replays it.
///
/// `in_service_due_at` is the instant the clock owes the space its move out of `Pending`;
/// it is stored, never slept on, and materialised by whichever reporting call first runs at
/// or after it ([`Space::settle_status`]).
///
/// The members the request did not carry stay `None`: the model documents no default for
/// any of them, so a reader reports exactly what the create sent rather than a value nobody
/// asked for. `tags` is stored so a later `ListTags` reports what the create carried.
#[derive(Debug, Clone)]
pub struct Space {
    pub arn: String,
    /// The domain this space belongs to, i.e. the `DomainId` the create named. Held on the
    /// record as well as in the key so a sweep over the store can tell which spaces hang
    /// off a domain without re-deriving the key.
    pub domain_id: String,
    /// The name as the create carried it, which a reader replays.
    pub name: String,
    pub display_name: Option<String>,
    pub space_settings: Option<SpaceSettings>,
    pub ownership_settings: Option<OwnershipSettings>,
    pub space_sharing_settings: Option<SpaceSharingSettings>,
    /// The space's Studio address ([`studio_space_url`]), resolved once on the write path
    /// from the domain's own, so every reader reports the one address.
    pub url: String,
    pub tags: Vec<Tag>,
    pub status: String,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub in_service_due_at: SystemTime,
    /// The instant the clock owes this space's removal, set by [`Space::begin_deletion`];
    /// `None` while no `DeleteSpace` has asked for one. Past it the space is gone for every
    /// caller ([`Space::is_released`]), whether or not a sweep has dropped the record yet —
    /// the way [`UserProfile::removal_due_at`] carries the same fact for the other member of
    /// the domain.
    pub removal_due_at: Option<SystemTime>,
}

impl Space {
    /// Materialise the `Pending` -> `InService` transition the clock already owes, the way
    /// [`UserProfile::settle_status`] does for the other member of the domain: it was due at
    /// `in_service_due_at` whether or not anybody read the space, so the observer makes no
    /// difference to the result, and `LastModifiedTime` moves to the instant the transition
    /// was due rather than to "now".
    ///
    /// Answers whether the transition happened on this call, so the caller can record it.
    pub fn settle_status(&mut self, now: SystemTime) -> bool {
        if self.status == SPACE_STATUS_PENDING && now >= self.in_service_due_at {
            self.last_modified_time = self.in_service_due_at;
            self.status = SPACE_STATUS_IN_SERVICE.to_string();
            return true;
        }
        false
    }

    /// Start the removal `DeleteSpace` asked for: the space reports `Deleting`, and the
    /// clock owes its removal [`SPACE_RELEASED_AFTER`] later.
    ///
    /// The record is NOT dropped here: `SpaceStatus` carries `Deleting` and SageMaker
    /// answers it until the removal finishes, so the transient status is observable and the
    /// removal is the clock's — stored as an instant and never slept on. The status change
    /// is a change to the space, so it moves `last_modified_time` too.
    pub fn begin_deletion(&mut self, now: SystemTime) {
        self.status = SPACE_STATUS_DELETING.to_string();
        self.last_modified_time = now;
        self.removal_due_at = Some(now + SPACE_RELEASED_AFTER);
    }

    /// Whether a `DeleteSpace` has asked for this space's removal, i.e. it is `Deleting`
    /// and on its way out.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// Whether the removal the clock owed has completed, i.e. the space is gone.
    ///
    /// A lookup answers `None` for such a space, so no caller sees one whose removal was
    /// already due — the record itself is dropped by
    /// [`ComAmazonawsSagemakerSagemakerState::release_deleted_spaces`], which frees the
    /// quota slot it still held.
    pub fn is_released(&self, now: SystemTime) -> bool {
        self.removal_due_at.is_some_and(|due| now >= due)
    }
}

impl ComAmazonawsSagemakerSagemakerState {
    /// Whether another space fits, i.e. the store is below [`SPACE_QUOTA`].
    pub fn spaces_below_quota(&self) -> bool {
        self.spaces.len() < SPACE_QUOTA
    }

    /// The space a `(DomainId, SpaceName)` pair names, or `None` when the domain holds none
    /// under that name.
    ///
    /// The one lookup every reader of that pair resolves it with, so no handler
    /// re-implements it: a space hangs off its domain, so one whose domain no longer
    /// resolves ([`Domain::is_released`]) resolves to nothing either — past that instant the
    /// domain and everything in it is gone, whether or not the records have been dropped yet
    /// ([`Self::release_deleted_domains`]). A space whose own removal the clock already
    /// completed resolves to nothing the same way ([`Space::is_released`]).
    ///
    /// `now` is the caller's instant, so one request weighs the store against ONE clock
    /// reading: the same instant its sweep and its transitions used.
    pub fn space_mut(
        &mut self,
        domain_id: &str,
        space_name: &str,
        now: SystemTime,
    ) -> Option<&mut Space> {
        if !self.domain_resolves(domain_id, now) {
            return None;
        }
        self.spaces
            .get_mut(&space_key(domain_id, space_name))
            .filter(|space| !space.is_released(now))
    }

    /// Whether the domain `domain_id` holds a space under `space_name`.
    ///
    /// Asked rather than resolved where only the existence matters, so a caller weighing a
    /// space as somebody else's member uses the same lookup a reader of the space would.
    pub fn holds_space(&self, domain_id: &str, space_name: &str, now: SystemTime) -> bool {
        self.domain_resolves(domain_id, now)
            && self
                .spaces
                .get(&space_key(domain_id, space_name))
                .is_some_and(|space| !space.is_released(now))
    }

    /// Drop every space whose removal the clock already completed.
    ///
    /// Materialises only what was already due: the removal was owed at the stored instant
    /// whether or not anybody called, and a lookup already answers `None` for such a space
    /// ([`Space::is_released`]), so this changes nothing a later caller sees — it frees the
    /// quota slot it still held. The call that asked for each removal already recorded it,
    /// so nothing is answered back.
    pub fn release_deleted_spaces(&mut self, now: SystemTime) {
        self.spaces.retain(|_, space| !space.is_released(now));
    }

    /// Drop every space of the domains `domain_ids`, i.e. the spaces of domains whose
    /// removal the clock already completed.
    ///
    /// Materialises only what was already due: a space of a gone domain already resolves to
    /// nothing ([`Self::space_mut`]), so this changes nothing a later caller sees — it frees
    /// the quota slots those spaces still held. Called from
    /// [`Self::release_deleted_domains`], which is where the domains themselves go, beside
    /// [`Self::release_domain_user_profiles`].
    pub fn release_domain_spaces(&mut self, domain_ids: &[String]) {
        if domain_ids.is_empty() {
            return;
        }
        self.spaces
            .retain(|_, space| !domain_ids.contains(&space.domain_id));
    }
}
