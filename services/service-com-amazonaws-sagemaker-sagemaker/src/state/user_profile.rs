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

/// The page `ListUserProfiles` serves when the request names no `MaxResults`: its
/// `MaxResults` doc says "The default value for MaxResults is 10". [`MAX_PAGE_SIZE`] still
/// caps the page above it, as it does for every listing here.
pub const USER_PROFILE_DEFAULT_PAGE_SIZE: i32 = 10;

/// `UserProfileSortKey`'s member that orders a listing by the instant each profile last
/// changed, as opposed to [`DEFAULT_SORT_BY`], the enum's other member and the documented
/// default (`smithy_query(query="shape", shape="UserProfileSortKey")`).
pub const USER_PROFILE_SORT_BY_LAST_MODIFIED_TIME: &str = "LastModifiedTime";

/// How a page token names one user profile: the domain it lives in and its name, the pair
/// that identifies it.
///
/// A listing's `NextToken` names the item the page stopped at ([`encode_page_token`]), so
/// the id has to identify one profile across calls without depending on the Region the
/// caller signed for — which a profile's ARN does carry.
pub fn user_profile_page_id(domain_id: &str, user_profile_name: &str) -> String {
    format!("{domain_id}/{user_profile_name}")
}

/// `UserProfileStatus`'s members a user profile of this emulator moves through. The
/// modelled enum also carries the update and failure states, which no implemented
/// operation reaches yet.
pub const USER_PROFILE_STATUS_PENDING: &str = "Pending";
/// See [`USER_PROFILE_STATUS_PENDING`].
pub const USER_PROFILE_STATUS_IN_SERVICE: &str = "InService";
/// See [`USER_PROFILE_STATUS_PENDING`]. What a profile reports from the
/// `DeleteUserProfile` that asked for its removal until the clock completes it
/// ([`UserProfile::begin_deletion`]).
pub const USER_PROFILE_STATUS_DELETING: &str = "Deleting";

/// How long after `CreateUserProfile` the clock owes the `Pending` -> `InService`
/// transition.
///
/// The same compressed, never-zero span the domain's own provisioning takes
/// ([`DOMAIN_IN_SERVICE_AFTER`]): a caller that describes the profile straight after
/// creating it still observes `Pending`, and one that waits observes `InService`. The due
/// instant is stored rather than slept on ([`UserProfile::settle_status`]).
pub const USER_PROFILE_IN_SERVICE_AFTER: Duration = DOMAIN_IN_SERVICE_AFTER;

/// How long after `DeleteUserProfile` the clock owes the `Deleting` -> gone transition of a
/// user profile.
///
/// The same compressed, never-zero span the domain's own teardown takes
/// ([`DOMAIN_RELEASED_AFTER`]): a caller that describes the profile straight after asking
/// for its deletion still observes `Deleting`, and one that polls observes it gone. The due
/// instant is stored rather than slept on ([`UserProfile::is_released`]).
pub const USER_PROFILE_RELEASED_AFTER: Duration = DOMAIN_RELEASED_AFTER;

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

/// How the operations whose SUBJECT is the profile itself state a `(DomainId,
/// UserProfileName)` pair this account holds no profile under: the entity, then the ARN the
/// requested pair resolves to in the Region the request signed for IN SQUARE BRACKETS, then
/// `does not exist.` — the sentence the Studio family's readers word this fact with, as
/// [`crate::state::domain_not_found`] does for the domain.
///
/// Distinct from [`user_profile_not_found`] on purpose, the way the domain's two sentences
/// are: AWS words the one fact for the reader it answers. `DescribeUserProfile` and
/// `DeleteUserProfile` address the PROFILE, so they name it by the ARN their request
/// resolves to, while an operation whose subject is something else and that merely resolves
/// the profile as a member (`CreateApp`'s owner) names the value the caller passed.
pub fn user_profile_arn_not_found(region: &str, domain_id: &str, user_profile_name: &str) -> String {
    format!(
        "UserProfile [{}] does not exist.",
        resource_arn(
            region,
            USER_PROFILE_ARN_KIND,
            &format!("{domain_id}/{user_profile_name}")
        )
    )
}

/// How `CreateUserProfile` states that the domain already holds a profile under the name
/// the request carried: the name IN SINGLE QUOTES, then the `DomainId` it is taken in —
/// recorded AWS behaviour, whose whole sentence is fixed text but for those two values.
///
/// The domain is named by the bare `d-…` id the request supplied rather than by an ARN,
/// which is also what tells this collision apart from the one an app's own "already in
/// use" refusal reports.
pub fn user_profile_already_exists(user_profile_name: &str, domain_id: &str) -> String {
    studio_member_already_exists(USER_PROFILE_ENTITY, user_profile_name, domain_id)
}

/// The entity label the Studio refusals name a user profile by, i.e. the subject of
/// [`crate::state::domain_not_in_service_refusal`] when the resource being created is a
/// profile.
pub const USER_PROFILE_ENTITY: &str = "UserProfile";

/// How `DeleteUserProfile` states that the profile cannot take the removal asked of it
/// while the service is already removing it: the same subject form
/// [`user_profile_arn_not_found`] names the profile with, then the status that stands in the
/// way — the wording [`crate::state::domain_not_deletable`] uses for the domain it lives
/// in.
pub fn user_profile_not_deletable(arn: &str, status: &str) -> String {
    studio_resource_not_deletable(USER_PROFILE_ENTITY, arn, status)
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
    /// The instant the clock owes this profile's removal, set by
    /// [`UserProfile::begin_deletion`]; `None` while no `DeleteUserProfile` has asked for
    /// one. Past it the profile is gone for every caller ([`UserProfile::is_released`]),
    /// whether or not a sweep has dropped the record yet — the way
    /// [`Domain::removal_due_at`] carries the same fact for the domain it lives in.
    pub removal_due_at: Option<SystemTime>,
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

    /// Start the removal `DeleteUserProfile` asked for: the profile reports `Deleting`, and
    /// the clock owes its removal [`USER_PROFILE_RELEASED_AFTER`] later.
    ///
    /// The record is NOT dropped here: `UserProfileStatus` carries `Deleting` and SageMaker
    /// answers it until the removal finishes, so the transient status is observable and the
    /// removal is the clock's — stored as an instant and never slept on. The status change
    /// is a change to the profile, so it moves `last_modified_time` too.
    pub fn begin_deletion(&mut self, now: SystemTime) {
        self.status = USER_PROFILE_STATUS_DELETING.to_string();
        self.last_modified_time = now;
        self.removal_due_at = Some(now + USER_PROFILE_RELEASED_AFTER);
    }

    /// Whether a `DeleteUserProfile` has asked for this profile's removal, i.e. it is
    /// `Deleting` and on its way out.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// Whether the removal the clock owed has completed, i.e. the profile is gone.
    ///
    /// A lookup answers `None` for such a profile, so no caller sees one whose removal was
    /// already due — the record itself is dropped by
    /// [`ComAmazonawsSagemakerSagemakerState::release_deleted_user_profiles`], which frees
    /// the quota slot it still held.
    pub fn is_released(&self, now: SystemTime) -> bool {
        self.removal_due_at.is_some_and(|due| now >= due)
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
    /// dropped yet ([`Self::release_deleted_domains`]). A profile whose own removal the
    /// clock already completed resolves to nothing the same way
    /// ([`UserProfile::is_released`]).
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
            .filter(|profile| !profile.is_released(now))
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
                .get(&user_profile_key(domain_id, user_profile_name))
                .is_some_and(|profile| !profile.is_released(now))
    }

    /// Drop every user profile whose removal the clock already completed, and with it the
    /// retained records of the apps that hung off it.
    ///
    /// Materialises only what was already due: the removal was owed at the stored instant
    /// whether or not anybody called, and a lookup already answers `None` for such a profile
    /// ([`UserProfile::is_released`]), so this changes nothing a later caller sees — it
    /// frees the quota slot it still held. The call that asked for each removal already
    /// recorded it, so nothing is answered back.
    pub fn release_deleted_user_profiles(&mut self, now: SystemTime) {
        let released: Vec<(String, String)> = self
            .user_profiles
            .values()
            .filter(|profile| profile.is_released(now))
            .map(|profile| (profile.domain_id.clone(), profile.name.clone()))
            .collect();
        self.user_profiles
            .retain(|_, profile| !profile.is_released(now));
        self.release_owner_apps(&released);
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
