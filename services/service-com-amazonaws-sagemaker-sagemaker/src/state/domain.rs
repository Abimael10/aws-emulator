// SPDX-License-Identifier: CC0-1.0

//! The STUDIO DOMAIN slice of this service's state: the domain `CreateDomain` writes,
//! `DescribeDomain` replays, `ListDomains` pages over and `DeleteDomain` tears down, with
//! its ARN kind, its id mint, its Studio address, its statuses, the clock spans it moves
//! on, its store bound and the sentences its refusals are worded with.
//!
//! Its own file, like the other resource slices here, so the parent module stays inside
//! the project's file-length limit. Every name is re-exported from [`super`], so a
//! handler keeps naming `crate::state::…`.

use super::*;
use crate::ops::types::{DefaultSpaceSettings, DomainSettings, UserSettings};

/// The ARN resource kind a domain carries, i.e. the `<kind>` in [`resource_arn`]. Fixed
/// by `DomainArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:domain/`), and named once here so
/// the create and every reader mint and match the same ARN.
pub const DOMAIN_ARN_KIND: &str = "domain";

/// See [`MODEL_QUOTA`]. Bounds the domain store the same way, and is what `CreateDomain`
/// answers `ResourceLimitExceeded` past, so a caller creating domain after domain cannot
/// grow the store without end.
pub const DOMAIN_QUOTA: usize = 1_000;

/// `DomainStatus`'s members a domain of this emulator moves through. The modelled enum
/// also carries the update, delete and failure states, which no implemented operation
/// reaches yet.
pub const DOMAIN_STATUS_PENDING: &str = "Pending";
/// See [`DOMAIN_STATUS_PENDING`].
pub const DOMAIN_STATUS_IN_SERVICE: &str = "InService";
/// See [`DOMAIN_STATUS_PENDING`]. The terminal status a domain whose provisioning could not
/// complete settles in — a placement the account's network does not admit
/// ([`crate::state::domain_network`]) — beside the `FailureReason` that states why.
pub const DOMAIN_STATUS_FAILED: &str = "Failed";
/// See [`DOMAIN_STATUS_PENDING`]. What a domain reports from the `DeleteDomain` that asked
/// for its deletion until the clock completes the teardown ([`Domain::begin_deletion`]).
pub const DOMAIN_STATUS_DELETING: &str = "Deleting";

/// How long after `CreateDomain` the clock owes the `Pending` -> `InService` transition.
///
/// Three seconds, so a caller that describes or lists the domain straight after creating
/// it still observes `Pending`, and one that waits observes `InService`. Real SageMaker
/// takes minutes to provision a Studio domain; the span is compressed, never zero, and
/// the due instant is stored rather than slept on ([`Domain::settle_status`]).
pub const DOMAIN_IN_SERVICE_AFTER: Duration = Duration::from_secs(3);

/// How long after `DeleteDomain` the clock owes the `Deleting` -> gone transition of a
/// domain.
///
/// The same compressed, never-zero span as [`DOMAIN_IN_SERVICE_AFTER`]: a caller that
/// describes the domain straight after asking for its deletion still observes `Deleting`,
/// and one that polls observes it gone. Real SageMaker takes minutes to tear a Studio
/// domain down, so the span is compressed; the due instant is stored rather than slept on
/// ([`Domain::is_released`]).
pub const DOMAIN_RELEASED_AFTER: Duration = DOMAIN_IN_SERVICE_AFTER;

/// The page `ListDomains` serves when the request names no `MaxResults`: its
/// `MaxResults` doc says "The default value for MaxResults is 10". [`MAX_PAGE_SIZE`]
/// still caps the page above it, as it does for every listing here.
pub const DOMAIN_DEFAULT_PAGE_SIZE: i32 = 10;

/// `AppNetworkAccessType`'s documented default: "Specifies the VPC used for non-EFS
/// traffic. The default value is PublicInternetOnly."
pub const DEFAULT_APP_NETWORK_ACCESS_TYPE: &str = "PublicInternetOnly";

/// `HomeEfsFileSystemCreation`'s documented default: "Indicates whether to create a home
/// EFS file system for the domain. Defaults to Enabled."
pub const DEFAULT_HOME_EFS_FILE_SYSTEM_CREATION: &str = "Enabled";

/// `TagPropagation`'s documented default: "Indicates whether custom tag propagation is
/// supported for the domain. Defaults to DISABLED."
pub const DEFAULT_TAG_PROPAGATION: &str = "DISABLED";

/// A fresh `DomainId`: `d-` and the twelve lowercase characters the modelled `DomainId`
/// pattern admits after it (`^d-(-*[a-z0-9]){1,61}$`).
///
/// The caller never supplies it — a domain is addressed by the id SageMaker mints, so the
/// id is the server's to generate and is derived from nothing in the request. Same scheme
/// as every other id in this crate ([`mint_short_id`]), whose digits are lowercase hex
/// and so inside this pattern's alphabet.
pub fn mint_domain_id() -> String {
    format!("d-{}", mint_short_id())
}

/// Where the Studio domain `domain_id` is served in `region`: the host SageMaker gives
/// one Studio domain, which `CreateDomain`, `DescribeDomain` and `ListDomains` all report
/// as the domain's `Url`.
///
/// Derived from the domain's own id and the Region the caller signed for, so it is stable
/// across reads and distinct per domain — the same way [`notebook_instance_url`] gives one
/// notebook instance its host.
pub fn studio_domain_url(region: &str, domain_id: &str) -> String {
    format!("https://{domain_id}.studio.{region}.sagemaker.aws")
}

/// How the domain operations state a `DomainId` this account holds no domain under: the
/// entity, then the ARN the requested id resolves to in the Region the request signed for
/// IN SQUARE BRACKETS, then `does not exist.` — the sentence the Studio family of
/// resources words this fact with, closing period included, as an app's own not-found
/// refusal does (`App [<arn>] does not exist.`).
///
/// Stated once here so the reader and whichever update or delete resolves the same id
/// later cannot word the one fact differently.
pub fn domain_not_found(region: &str, domain_id: &str) -> String {
    format!(
        "Domain [{}] does not exist.",
        resource_arn(region, DOMAIN_ARN_KIND, domain_id)
    )
}

/// How an operation whose subject is a resource INSIDE the domain states a `DomainId` this
/// account holds no domain under: the entity, then the id THE REQUEST SUPPLIED in square
/// brackets, then `does not exist` — recorded AWS behaviour, closing period included, i.e.
/// without one.
///
/// Distinct from [`domain_not_found`] on purpose: AWS words the one fact for the reader it
/// answers. An operation addressing the DOMAIN itself (`DescribeDomain`, `DeleteDomain`)
/// names it by the ARN its id resolves to, while one whose subject is a member of the
/// domain (`CreateUserProfile`) names the id the caller passed, which is all that caller
/// said about the domain.
pub fn domain_id_not_found(domain_id: &str) -> String {
    format!("Domain [{domain_id}] does not exist")
}

/// How the domain operations state that a domain cannot take the change asked of it while
/// the service is already moving it: the entity, then the ARN the request resolves to IN
/// SQUARE BRACKETS — the subject form [`domain_not_found`] names the same resource with —
/// then the status that stands in the way.
///
/// What a delete of a domain the service is already tearing down is answered with, so a
/// repeat of that call reports the work in flight rather than claiming a second deletion.
pub fn domain_not_deletable(arn: &str, status: &str) -> String {
    studio_resource_not_deletable(DOMAIN_ENTITY, arn, status)
}

/// The entity label the Studio refusals name a domain by, i.e. the subject of
/// [`studio_resource_not_deletable`] when the resource being deleted is the domain itself.
pub const DOMAIN_ENTITY: &str = "Domain";

/// How a Studio operation states that the resource it addresses cannot take the removal
/// asked of it while the service is already removing it: the entity, then the ARN the
/// request resolves to IN SQUARE BRACKETS, then the status that stands in the way.
///
/// One sentence for the whole family — the domain and every member it holds — so a repeat
/// of any of their deletes reports the work in flight the same way, with only the entity
/// and the resource changing.
pub fn studio_resource_not_deletable(entity: &str, arn: &str, status: &str) -> String {
    format!("{entity} [{arn}] cannot be deleted while it is in {status} status.")
}

/// How a Studio create states that the domain already holds a member under the name the
/// request carried: the entity, then the name IN SINGLE QUOTES, then the `DomainId` it is
/// taken in — recorded AWS behaviour for the user profile's own collision, whose whole
/// sentence is fixed text but for those values.
///
/// The domain is named by the bare `d-…` id the request supplied rather than by an ARN, the
/// way every refusal a member-create words names the domain it was handed
/// ([`domain_id_not_found`]). One sentence for every member of the domain, so a second
/// create of a taken name is refused the same way whichever member it names.
pub fn studio_member_already_exists(entity: &str, name: &str, domain_id: &str) -> String {
    format!("A {entity} with name '{name}' already exists in Domain {domain_id}.")
}

/// How `DeleteDomain` states that the domain still holds Studio apps, which have to be
/// deleted first: the same bracketed-ARN subject as [`domain_not_found`], then the apps
/// that have not been deleted and so stand in the way.
///
/// Named for the sentence rather than for the question, so it cannot be mistaken for
/// [`ComAmazonawsSagemakerSagemakerState::domain_holds_apps`], which is what ASKS it.
pub fn domain_holds_apps_refusal(arn: &str) -> String {
    format!("Domain [{arn}] has non-deleted Apps.")
}

/// How `DeleteDomain` states that the domain still holds Studio user profiles, which have
/// to be deleted first ("All of the members of the domain must be deleted first"): the
/// same bracketed-ARN subject as [`domain_not_found`], then the members that stand in the
/// way.
///
/// Named for the sentence rather than for the question, like
/// [`domain_holds_apps_refusal`] beside it.
pub fn domain_holds_user_profiles_refusal(arn: &str) -> String {
    format!("Unable to delete Domain [{arn}] because UserProfile(s) are associated with it.")
}

/// How the Studio operations state that a resource cannot be created in a domain the
/// service has not finished provisioning: the would-be resource's ARN IN SQUARE BRACKETS,
/// then the domain's, then the status the create needs it in.
///
/// A domain only admits members once it reports [`DOMAIN_STATUS_IN_SERVICE`], so this is
/// what a create addressing a `Pending` (or failed, or tearing-down) domain is refused
/// with. Stated once here so every create that hangs a resource off a domain words the one
/// fact the same way.
pub fn domain_not_in_service_refusal(entity: &str, resource_arn: &str, domain_arn: &str) -> String {
    format!(
        "Unable to create {entity} [{resource_arn}] because Domain [{domain_arn}] is not in \
         {DOMAIN_STATUS_IN_SERVICE}."
    )
}

/// A Studio domain created by `CreateDomain`, as `DescribeDomain` replays it and
/// `ListDomains` summarises it.
///
/// `in_service_due_at` is the instant the clock owes this domain its move out of
/// `Pending` — into `InService`, or into [`DOMAIN_STATUS_FAILED`] where the create
/// resolved a placement the account's network does not admit; it is stored, never slept
/// on, and materialised by whichever reporting call first runs at or after it
/// ([`Domain::settle_status`]).
///
/// The members the request did not carry stay `None`: the model documents a default for
/// `AppNetworkAccessType`, `HomeEfsFileSystemCreation` and `TagPropagation` only, so
/// those three are resolved ONCE here on the write path and every other optional member
/// is reported exactly as the create sent it rather than as a value nobody asked for.
/// `tags` is stored so a later `ListTags` reports what the create carried.
#[derive(Debug, Clone)]
pub struct Domain {
    pub id: String,
    pub arn: String,
    pub name: String,
    pub url: String,
    pub auth_mode: String,
    pub default_user_settings: UserSettings,
    pub domain_settings: Option<DomainSettings>,
    pub default_space_settings: Option<DefaultSpaceSettings>,
    pub subnet_ids: Option<Vec<String>>,
    pub vpc_id: Option<String>,
    pub kms_key_id: Option<String>,
    pub home_efs_file_system_kms_key_id: Option<String>,
    pub app_network_access_type: String,
    pub app_security_group_management: Option<String>,
    pub home_efs_file_system_creation: String,
    pub tag_propagation: String,
    pub status: String,
    /// Why this domain is in a failure status, i.e. the `FailureReason` a reader reports
    /// beside the status. `None` until the clock settles a domain whose provisioning could
    /// not complete, because only a domain in a failure status has a reason to report.
    pub failure_reason: Option<String>,
    /// Why this domain's provisioning will NOT complete, as the create resolved it against
    /// the account's own network ([`crate::state::domain_network_failure_reason`]), or `None`
    /// for a placement that holds together. Kept apart from [`Domain::failure_reason`]
    /// because it is what the clock still owes the domain rather than what the domain
    /// already reports: the create is served and the domain is `Pending` either way, and
    /// this decides WHICH status [`Domain::settle_status`] materialises.
    pub creation_failure_reason: Option<String>,
    pub tags: Vec<Tag>,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub in_service_due_at: SystemTime,
    /// The instant the clock owes this domain's removal, set by
    /// [`Domain::begin_deletion`]; `None` while no `DeleteDomain` has asked for one. Past
    /// it the domain is gone for every caller ([`Domain::is_released`]), whether or not a
    /// sweep has dropped the record yet — the way [`FeatureGroup::removal_due_at`] carries
    /// the same fact.
    pub removal_due_at: Option<SystemTime>,
}

impl Domain {
    /// Materialise the transition out of `Pending` the clock already owes, the way
    /// [`Endpoint::settle_status`] does for an endpoint: it was due at `in_service_due_at`
    /// whether or not anybody read the domain, so the observer makes no difference to the
    /// result, and `LastModifiedTime` moves to the instant the transition was due rather
    /// than to "now".
    ///
    /// WHICH status it lands in is what the create already resolved: `InService` for a
    /// domain whose provisioning completes, and [`DOMAIN_STATUS_FAILED`] with the
    /// `FailureReason` beside it for one whose placement the account's network does not
    /// admit ([`Domain::creation_failure_reason`]).
    ///
    /// Answers whether the transition happened on this call, so the caller can record it.
    pub fn settle_status(&mut self, now: SystemTime) -> bool {
        if self.status == DOMAIN_STATUS_PENDING && now >= self.in_service_due_at {
            self.last_modified_time = self.in_service_due_at;
            // What the create resolved about the domain's placement decides WHICH status the
            // clock owed: a domain whose network the account does not admit fails with the
            // reason that create recorded, and never comes up.
            if let Some(reason) = self.creation_failure_reason.clone() {
                self.status = DOMAIN_STATUS_FAILED.to_string();
                self.failure_reason = Some(reason);
                return true;
            }
            self.status = DOMAIN_STATUS_IN_SERVICE.to_string();
            return true;
        }
        false
    }

    /// Start the teardown `DeleteDomain` asked for: the domain reports `Deleting`, and the
    /// clock owes its removal [`DOMAIN_RELEASED_AFTER`] later.
    ///
    /// The record is NOT dropped here: `DomainStatus` carries `Deleting` and SageMaker
    /// answers it until the teardown finishes, so the transient status is observable and
    /// the removal is the clock's — stored as an instant and never slept on. The status
    /// change is a change to the domain, so it moves `last_modified_time` too.
    pub fn begin_deletion(&mut self, now: SystemTime) {
        self.status = DOMAIN_STATUS_DELETING.to_string();
        self.last_modified_time = now;
        self.removal_due_at = Some(now + DOMAIN_RELEASED_AFTER);
    }

    /// Whether a `DeleteDomain` has asked for this domain's deletion, i.e. it is
    /// `Deleting` and on its way out.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// Whether the removal the clock owed has completed, i.e. the domain is gone.
    ///
    /// A lookup answers `None` for such a domain and a listing leaves it out, so no caller
    /// sees a domain whose removal was already due — the record itself is dropped by
    /// [`ComAmazonawsSagemakerSagemakerState::release_deleted_domains`], which frees the
    /// quota slot it still held.
    pub fn is_released(&self, now: SystemTime) -> bool {
        self.removal_due_at.is_some_and(|due| now >= due)
    }
}

impl ComAmazonawsSagemakerSagemakerState {
    /// Whether another domain fits, i.e. the store is below [`DOMAIN_QUOTA`].
    pub fn domains_below_quota(&self) -> bool {
        self.domains.len() < DOMAIN_QUOTA
    }

    /// The domain a `DomainId` names, or `None` when this account holds none under it.
    ///
    /// The one lookup every domain reader resolves that member with, so no handler
    /// re-implements it: a domain whose removal the clock already completed resolves to
    /// nothing — past that instant the domain is gone whether or not the record has been
    /// dropped yet ([`Domain::is_released`]).
    ///
    /// `now` is the caller's instant, so one request weighs the store against ONE clock
    /// reading: the same instant its sweep and its transitions used.
    pub fn domain_mut(&mut self, domain_id: &str, now: SystemTime) -> Option<&mut Domain> {
        self.domains
            .get_mut(domain_id)
            .filter(|domain| !domain.is_released(now))
    }

    /// Whether the `DomainId` `domain_id` still resolves to a domain of this account, i.e.
    /// a reader would answer for it.
    ///
    /// Asked rather than resolved where only the existence matters — what a resource that
    /// hangs off the domain ([`UserProfile`]) is weighed against, so the one rule about
    /// which ids resolve ([`Self::domain_mut`]) holds for those resources too.
    pub fn domain_resolves(&self, domain_id: &str, now: SystemTime) -> bool {
        self.domains
            .get(domain_id)
            .is_some_and(|domain| !domain.is_released(now))
    }

    /// Drop every domain whose removal the clock already completed, and with it everything
    /// that hung off that domain.
    ///
    /// Materialises only what was already due: the removal was owed at the stored instant
    /// whether or not anybody called, and a lookup already answers `None` for such a
    /// domain ([`Domain::is_released`]) and for its user profiles
    /// ([`Self::user_profile_mut`]), so this changes nothing a later caller sees — it
    /// frees the quota slots the gone domain and its profiles still held. The call that
    /// asked for each removal already recorded it, so nothing is answered back.
    ///
    /// The profiles go WITH the domain rather than standing in the way of its teardown:
    /// `DeleteDomain` already refuses while the domain holds a profile no
    /// `DeleteUserProfile` has released ([`Self::domain_holds_user_profiles`]), so what is
    /// left here is a profile whose own removal the caller asked for first — it goes when
    /// the domain does, and so do the spaces and the apps that hung off it, so no record
    /// addresses a domain no reader resolves.
    pub fn release_deleted_domains(&mut self, now: SystemTime) {
        let released: Vec<String> = self
            .domains
            .values()
            .filter(|domain| domain.is_released(now))
            .map(|domain| domain.id.clone())
            .collect();
        for id in &released {
            self.domains.remove(id);
        }
        self.release_domain_user_profiles(&released);
        self.release_domain_spaces(&released);
        self.release_domain_apps(&released);
    }

    /// Whether the domain `domain_id` still holds Studio apps, i.e. apps created in it
    /// that no `DeleteApp` has torn down.
    ///
    /// An app hangs off the domain it was created in ([`AppKey::domain_id`]), so this is
    /// what `DeleteDomain` weighs before tearing the domain down: releasing the domain
    /// while its apps stayed behind would leave records addressing a domain no reader
    /// resolves. The retained record of a deleted app ([`App::is_released`]) is not a
    /// running app, so it stands in nothing's way.
    pub fn domain_holds_apps(&self, domain_id: &str) -> bool {
        self.apps
            .iter()
            .any(|(key, app)| key.domain_id == domain_id && !app.is_released())
    }

    /// Whether the domain `domain_id` still holds Studio user profiles, i.e. profiles
    /// created in it that no `DeleteUserProfile` has released.
    ///
    /// A user profile is "a single user within a domain", so it is one of the members AWS
    /// requires to be deleted before the domain itself: this is what `DeleteDomain` weighs
    /// beside [`Self::domain_holds_apps`]. A profile already on its way out
    /// ([`UserProfile::is_deleting`]) is not counted — the caller has asked for its
    /// removal, which is the order the teardown has to follow.
    pub fn domain_holds_user_profiles(&self, domain_id: &str) -> bool {
        self.user_profiles
            .values()
            .any(|profile| profile.domain_id == domain_id && !profile.is_deleting())
    }
}
