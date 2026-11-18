// SPDX-License-Identifier: CC0-1.0

//! The STUDIO USER PROFILE slice of this service's state: the profile `CreateUserProfile`
//! writes and `DescribeUserProfile` replays, with its ARN kind, its store key, its
//! statuses, the clock span it moves on, its store bound and the sentences its refusals
//! are worded with.
//!
//! A user profile is the OWNER a Studio app hangs off (`CreateApp`'s `UserProfileName`),
//! so this slice is also what resolves that member: an app can only be created for a
//! profile the domain actually holds.
//!
//! Its own file, like the other resource slices here, so the parent module stays inside
//! the project's file-length limit. Every name is re-exported from [`super`], so a
//! handler keeps naming `crate::state::…`.

use super::*;
use crate::ops::types::UserSettings;

/// The ARN resource kind a user profile carries, i.e. the `<kind>` in [`resource_arn`].
/// Fixed by `UserProfileArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:user-profile/`), and named once here
/// so the create and every reader mint and match the same ARN.
pub const USER_PROFILE_ARN_KIND: &str = "user-profile";

/// See [`DOMAIN_QUOTA`]. Bounds the user-profile store the same way, and is what
/// `CreateUserProfile` answers `ResourceLimitExceeded` past, so a caller creating profile
/// after profile cannot grow the store without end.
pub const USER_PROFILE_QUOTA: usize = 1_000;

/// `UserProfileStatus`'s members a user profile of this emulator moves through. The
/// modelled enum also carries the update, delete and failure states, which no implemented
/// operation reaches yet.
pub const USER_PROFILE_STATUS_PENDING: &str = "Pending";
/// See [`USER_PROFILE_STATUS_PENDING`].
pub const USER_PROFILE_STATUS_IN_SERVICE: &str = "InService";

/// How long after `CreateUserProfile` the clock owes the `Pending` -> `InService`
/// transition.
///
/// The same compressed, never-zero span the domain's own provisioning takes
/// ([`DOMAIN_IN_SERVICE_AFTER`]): a caller that describes the profile straight after
/// creating it still observes `Pending`, and one that waits observes `InService`. The due
/// instant is stored rather than slept on ([`UserProfile::settle_status`]).
pub const USER_PROFILE_IN_SERVICE_AFTER: Duration = DOMAIN_IN_SERVICE_AFTER;

/// The store key a `(DomainId, UserProfileName)` pair maps to.
///
/// A user profile "represents a single user within a domain", so the domain is part of its
/// identity: the same name in two domains is two profiles. `UserProfileName`'s model doc
/// says "This value is not case sensitive", so the name half is case-folded with
/// [`entity_key`] while the record keeps the casing the caller sent.
pub fn user_profile_key(domain_id: &str, user_profile_name: &str) -> (String, String) {
    (domain_id.to_string(), entity_key(user_profile_name))
}

/// How the Studio operations state that a domain holds no user profile under the name a
/// request names: the entity, then the name the request carried IN SQUARE BRACKETS, then
/// `does not exist.` — recorded AWS behaviour for the `CreateApp` that names an absent
/// user profile, whose whole message is fixed text but for that name.
///
/// Stated once here so the app create, the reader and whatever resolves the same member
/// later cannot word the one fact differently.
pub fn user_profile_not_found(user_profile_name: &str) -> String {
    format!("UserProfile [{user_profile_name}] does not exist.")
}

/// How `CreateUserProfile` states that the profile it was asked to create is already
/// there: the ARN the request resolves to IN SQUARE BRACKETS, then `already exists.` — the
/// subject form the Studio family's "already exists" refusals use (`App [<arn>] already
/// exists.`), which is what `ResourceInUse` reports for a create whose resource is taken.
pub fn user_profile_already_exists(arn: &str) -> String {
    format!("UserProfile [{arn}] already exists.")
}

/// `AuthMode`'s member that means the domain authenticates its members against IAM
/// Identity Center, i.e. the one the single-sign-on members of a user profile belong to.
/// The enum's other member is `IAM` (`smithy_query(query="shape", shape="AuthMode")`).
pub const AUTH_MODE_IAM_IDENTITY_CENTER: &str = "SSO";

/// The two members of a `CreateUserProfile` request that carry the user's identity in the
/// directory the domain authenticates against, in the order the refusals below name them.
pub const NAMED_SINGLE_SIGN_ON_MEMBERS: &[&str] =
    &["SingleSignOnUserIdentifier", "SingleSignOnUserValue"];

/// Why a user profile of a domain that authenticates against IAM Identity Center cannot be
/// created without the user's identity in that directory: both members' model docs make
/// each one "required" for such a domain, so the request has to carry them.
///
/// Worded the way the boundary validator words a refusal — naming the members the caller
/// has to add — because the fact lives in those members' docs rather than in a modelled
/// constraint.
pub fn single_sign_on_members_required() -> String {
    format!(
        "{} are required when the Domain's AuthMode is IAM Identity Center.",
        NAMED_SINGLE_SIGN_ON_MEMBERS.join(" and ")
    )
}

/// Why a user profile of a domain that does NOT authenticate against IAM Identity Center
/// cannot carry the user's identity in that directory: both members' model docs say such a
/// field "cannot be specified" for such a domain.
///
/// Names exactly the member(s) the request carried, so a caller fixes the request from the
/// sentence alone.
pub fn single_sign_on_members_not_allowed(members: &[&str]) -> String {
    format!(
        "{} cannot be specified when the Domain's AuthMode is not IAM Identity Center.",
        members.join(" and ")
    )
}

/// A Studio user profile created by `CreateUserProfile`, as `DescribeUserProfile` replays
/// it.
///
/// `in_service_due_at` is the instant the clock owes the profile its move out of
/// `Pending`; it is stored, never slept on, and materialised by whichever reporting call
/// first runs at or after it ([`UserProfile::settle_status`]).
///
/// The members the request did not carry stay `None`: the model documents no default for
/// any of them, so a reader reports exactly what the create sent rather than a value
/// nobody asked for. `tags` is stored so a later `ListTags` reports what the create
/// carried.
#[derive(Debug, Clone)]
pub struct UserProfile {
    pub arn: String,
    /// The domain this profile belongs to, i.e. the `DomainId` the create named. Held on
    /// the record as well as in the key so a sweep over the store can tell which profiles
    /// hang off a domain without re-deriving the key.
    pub domain_id: String,
    /// The name as the caller spelled it, which a reader replays — the store key holds the
    /// case-folded form ([`user_profile_key`]).
    pub name: String,
    pub single_sign_on_user_identifier: Option<String>,
    pub single_sign_on_user_value: Option<String>,
    pub user_settings: Option<UserSettings>,
    pub tags: Vec<Tag>,
    pub status: String,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub in_service_due_at: SystemTime,
}

impl UserProfile {
    /// Materialise the `Pending` -> `InService` transition the clock already owes, the way
    /// [`Domain::settle_status`] does for the domain the profile lives in: it was due at
    /// `in_service_due_at` whether or not anybody read the profile, so the observer makes
    /// no difference to the result, and `LastModifiedTime` moves to the instant the
    /// transition was due rather than to "now".
    ///
    /// Answers whether the transition happened on this call, so the caller can record it.
    pub fn settle_status(&mut self, now: SystemTime) -> bool {
        if self.status == USER_PROFILE_STATUS_PENDING && now >= self.in_service_due_at {
            self.last_modified_time = self.in_service_due_at;
            self.status = USER_PROFILE_STATUS_IN_SERVICE.to_string();
            return true;
        }
        false
    }
}

impl ComAmazonawsSagemakerSagemakerState {
    /// Whether another user profile fits, i.e. the store is below [`USER_PROFILE_QUOTA`].
    pub fn user_profiles_below_quota(&self) -> bool {
        self.user_profiles.len() < USER_PROFILE_QUOTA
    }

    /// The user profile a `(DomainId, UserProfileName)` pair names, or `None` when the
    /// domain holds none under that name.
    ///
    /// The one lookup every reader of that pair resolves it with, so no handler
    /// re-implements it: a profile hangs off its domain, so one whose domain no longer
    /// resolves ([`Domain::is_released`]) resolves to nothing either — past that instant
    /// the domain and everything in it is gone, whether or not the records have been
    /// dropped yet ([`Self::release_deleted_domains`]).
    ///
    /// `now` is the caller's instant, so one request weighs the store against ONE clock
    /// reading: the same instant its sweep and its transitions used.
    pub fn user_profile_mut(
        &mut self,
        domain_id: &str,
        user_profile_name: &str,
        now: SystemTime,
    ) -> Option<&mut UserProfile> {
        if !self.domain_resolves(domain_id, now) {
            return None;
        }
        self.user_profiles
            .get_mut(&user_profile_key(domain_id, user_profile_name))
    }

    /// Whether the domain `domain_id` holds a user profile under `user_profile_name`, i.e.
    /// the owner a `CreateApp` names resolves.
    ///
    /// Asked rather than resolved where only the existence matters, so the app create
    /// weighs the owner with the same lookup a reader of the profile would use.
    pub fn holds_user_profile(
        &self,
        domain_id: &str,
        user_profile_name: &str,
        now: SystemTime,
    ) -> bool {
        self.domain_resolves(domain_id, now)
            && self
                .user_profiles
                .contains_key(&user_profile_key(domain_id, user_profile_name))
    }

    /// Drop every user profile of the domains `domain_ids`, i.e. the profiles of domains
    /// whose removal the clock already completed.
    ///
    /// Materialises only what was already due: a profile of a gone domain already resolves
    /// to nothing ([`Self::user_profile_mut`]), so this changes nothing a later caller sees
    /// — it frees the quota slots those profiles still held. Called from
    /// [`Self::release_deleted_domains`], which is where the domains themselves go.
    pub fn release_domain_user_profiles(&mut self, domain_ids: &[String]) {
        if domain_ids.is_empty() {
            return;
        }
        self.user_profiles
            .retain(|_, profile| !domain_ids.contains(&profile.domain_id));
    }
}
