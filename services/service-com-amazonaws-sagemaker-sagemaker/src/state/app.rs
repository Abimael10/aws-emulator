// SPDX-License-Identifier: CC0-1.0

//! The STUDIO APP slice of this service's state: the app `CreateApp` launches,
//! `DescribeApp` replays and `DeleteApp` tears down, with its key, its ARN, its statuses,
//! the clock spans it moves on, the window a deleted app's record stays readable for, its
//! store bound and the sentences its refusals are worded with.
//!
//! An app's whole lifecycle is clock-owed: it comes up `Pending` and reaches `InService`,
//! and a delete parks it in `Deleting` and settles it on `Deleted`. The record is RETAINED
//! past that terminal status for [`APP_RETAINED_AFTER`], so a client polling the teardown
//! reads the app's own status instead of a not-found refusal.
//!
//! Its own file, like the other resource slices here, so the parent module stays inside
//! the project's file-length limit. Every name is re-exported from [`super`], so a
//! handler keeps naming `crate::state::…`.

use super::*;

/// How long after `CreateApp` the clock owes the `Pending` -> `InService` transition.
///
/// Same contract as [`ENDPOINT_IN_SERVICE_AFTER`], whose compressed span this is: a caller
/// that describes the app straight after creating it still observes `Pending`, and one that
/// polls observes `InService`. Real Studio app launches take minutes; the span is
/// compressed, never zero, and the due instant is stored rather than slept on
/// ([`App::settle_status`]).
pub const APP_IN_SERVICE_AFTER: Duration = ENDPOINT_IN_SERVICE_AFTER;

/// How long after `DeleteApp` the clock owes the `Deleting` -> `Deleted` transition.
///
/// The same compressed, never-zero span the launch takes ([`APP_IN_SERVICE_AFTER`]): a
/// caller that describes the app straight after asking for its deletion still observes
/// `Deleting`, and one that polls observes `Deleted`. The due instant is stored rather
/// than slept on ([`App::settle_status`]).
pub const APP_DELETED_AFTER: Duration = APP_IN_SERVICE_AFTER;

/// How long a deleted app's record stays readable, counted from the `DeleteApp` that asked
/// for the teardown.
///
/// Recorded behaviour, which the refusal a second delete carries states in words: apps
/// "are available in the ListApps API results for only 24 hours after deletion". Past it
/// the record is gone and the key reads as one nothing ever created ([`App::is_purged`]).
pub const APP_RETAINED_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// See [`MODEL_QUOTA`].
pub const APP_QUOTA: usize = 1_000;

/// The page `ListApps` serves when the request names no `MaxResults`: its `MaxResults` doc
/// says "The default value for MaxResults is 10". [`MAX_PAGE_SIZE`] still caps the page
/// above it, as it does for every listing here.
pub const APP_DEFAULT_PAGE_SIZE: i32 = 10;

/// Why a `ListApps` naming both `UserProfileNameEquals` and `SpaceNameEquals` is refused:
/// each member's doc says the other cannot be set beside it, and an app has exactly one
/// owner, so such a search names an app that cannot exist.
pub const APP_SEARCH_OWNER_EXCLUSIVE: &str =
    "At most one of UserProfileNameEquals or SpaceNameEquals may be specified.";

/// How a page token names one app: the five members that identify it, in the order
/// [`app_arn`] splices them into the ARN.
///
/// A listing's `NextToken` names the item the page stopped at ([`encode_page_token`]), so
/// the id has to identify one app across calls without depending on the Region the caller
/// signed for — which an app's ARN does carry.
pub fn app_page_id(key: &AppKey) -> String {
    format!(
        "{}/{}/{}/{}",
        key.domain_id,
        key.owner.name(),
        key.app_type,
        key.app_name
    )
}

/// The most deleted apps whose records are kept inside [`APP_RETAINED_AFTER`].
///
/// The retention window is a day, so a caller creating and deleting app after app would
/// grow the store without end before any record fell due; the oldest deletions are
/// dropped past this bound ([`ComAmazonawsSagemakerSagemakerState::prune_deleted_apps`]).
/// Equal to [`APP_QUOTA`], so the store holds at most twice the apps one account can run.
pub const DELETED_APP_RECORDS_CAP: usize = APP_QUOTA;

/// `AppStatus`'s members this emulator moves an app through
/// (`smithy_query(query="shape", shape="AppStatus")`). The modelled enum also carries
/// `Failed`, which nothing here reaches: this emulator fails no launch.
pub const APP_STATUS_PENDING: &str = "Pending";
/// See [`APP_STATUS_PENDING`].
pub const APP_STATUS_IN_SERVICE: &str = "InService";
/// See [`APP_STATUS_PENDING`]. What an app reports from the `DeleteApp` that asked for its
/// teardown until the clock completes it ([`App::begin_deletion`]).
pub const APP_STATUS_DELETING: &str = "Deleting";
/// See [`APP_STATUS_PENDING`]. The terminal status a torn-down app settles in, reported for
/// as long as the record is retained ([`APP_RETAINED_AFTER`]).
pub const APP_STATUS_DELETED: &str = "Deleted";

/// The `AppStatus`es a new app can be launched over, i.e. the ones AWS's own refusal names
/// as the states to wait for ("wait until it is in Deleted or Failed status").
///
/// An app in any other status is still the resource a `CreateApp` for its key would have to
/// take over, which is what [`app_in_use_refusal`] reports.
pub const APP_RESTARTABLE_STATUSES: &[&str] = &[APP_STATUS_DELETED, "Failed"];

/// The `AppType`s the service has WITHDRAWN, i.e. the ones every `CreateApp` naming them is
/// refused over before an app is launched.
///
/// The member stays in the modelled enum (`smithy_query(query="shape", shape="AppType")`
/// still lists `KernelGateway`, and a stored app of that type is still read and deleted),
/// because the model marks nothing deprecated: it is the CREATE the service no longer
/// serves, which is recorded behaviour worded by [`deprecated_app_type_refusal`].
pub const DEPRECATED_APP_TYPES: &[&str] = &["KernelGateway"];

/// How `CreateApp` states that the `AppType` the request named is one the service has
/// withdrawn: the type the request carried IN SQUARE BRACKETS, then the sentence that says
/// it is gone. Everything but the type is fixed text for this error class.
pub fn deprecated_app_type_refusal(app_type: &str) -> String {
    format!("Invalid AppType [{app_type}]. This AppType has been deprecated.")
}

/// How `CreateApp` states that the five members the request names already address an app
/// the service is running: the app's ARN IN SQUARE BRACKETS, the status it is in now, and
/// the statuses a restart has to wait for.
///
/// The CURRENT status is what the caller has to act on, so it is read from the stored app
/// rather than fixed in the sentence.
pub fn app_in_use_refusal(arn: &str, status: &str) -> String {
    format!(
        "App [{arn}] is currently in [{status}] status. To restart the App, wait until it \
         is in Deleted or Failed status."
    )
}

/// How `DeleteApp` states that the app it was asked to delete is already on its way out:
/// the `AppName` the request carried IN SQUARE BRACKETS, then the sentence that says the
/// teardown is in flight. The name is the only variable part.
pub fn app_already_deleting_refusal(app_name: &str) -> String {
    format!("App [{app_name}] is already being deleted. Wait until the app is deleted.")
}

/// How `DeleteApp` states that the app it was asked to delete is already gone, i.e. its
/// teardown has settled and only the retained record is left: the `AppName` the request
/// carried IN SQUARE BRACKETS, then the sentence that says so and for how long the record
/// stays readable ([`APP_RETAINED_AFTER`]). The name is the only variable part.
pub fn app_already_deleted_refusal(app_name: &str) -> String {
    format!(
        "App [{app_name}] has already been deleted. For tracking purposes, apps are \
         available in the ListApps API results for only 24 hours after deletion."
    )
}

/// Which Studio principal a Studio app hangs off.
///
/// `CreateApp` / `DescribeApp` / `DeleteApp` all take `UserProfileName` and `SpaceName`
/// as optional members whose model docs say each must be set when the other is not
/// (`smithy_query(query="doc", shape="DeleteAppRequest", member="SpaceName")`), so
/// exactly one of the two identifies the app.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppOwner {
    /// `UserProfileName`.
    UserProfile(String),
    /// `SpaceName`.
    Space(String),
}

impl AppOwner {
    /// The owner's name, whichever member named it — the component an app ARN carries.
    pub fn name(&self) -> &str {
        match self {
            Self::UserProfile(name) | Self::Space(name) => name,
        }
    }

    /// `UserProfileName` as the app reports it, i.e. only for a user-profile app.
    pub fn user_profile_name(&self) -> Option<String> {
        match self {
            Self::UserProfile(name) => Some(name.clone()),
            Self::Space(_) => None,
        }
    }

    /// `SpaceName` as the app reports it, i.e. only for a space app.
    pub fn space_name(&self) -> Option<String> {
        match self {
            Self::Space(name) => Some(name.clone()),
            Self::UserProfile(_) => None,
        }
    }
}

/// What identifies one Studio app: the domain, the owning user profile or space, the
/// app type and the app name — the five members every app operation takes. Two apps
/// differing in any one of them are different apps, so this is the store's key.
///
/// Ordered (domain, owner, type, name) so the store has one stable order to list over.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppKey {
    pub domain_id: String,
    pub owner: AppOwner,
    pub app_type: String,
    pub app_name: String,
}

/// Why a request naming neither or both of `UserProfileName` and `SpaceName` is refused.
///
/// Both members are optional in the model, so the boundary validator passes such a
/// request; the rule lives in their docs ("If this value is not set, then
/// UserProfileName must be set", and the mirror of it), and an app whose owner is
/// unnamed — or named twice — cannot be resolved.
pub const APP_OWNER_REQUIRED: &str =
    "Exactly one of UserProfileName or SpaceName must be specified.";

/// The key the app operations' five input members name, or [`APP_OWNER_REQUIRED`] when
/// the owner is not exactly one of the user profile and the space.
///
/// Shared by `CreateApp`, `DescribeApp` and `DeleteApp` so all three resolve an app the
/// same way: a create and the reader or delete that names the same five members must
/// agree on the key, or the resource is unreachable.
pub fn app_key(
    domain_id: &str,
    user_profile_name: Option<&str>,
    space_name: Option<&str>,
    app_type: &str,
    app_name: &str,
) -> Result<AppKey, &'static str> {
    let owner = match (user_profile_name, space_name) {
        (Some(user_profile), None) => AppOwner::UserProfile(user_profile.to_string()),
        (None, Some(space)) => AppOwner::Space(space.to_string()),
        (Some(_), Some(_)) | (None, None) => return Err(APP_OWNER_REQUIRED),
    };
    Ok(AppKey {
        domain_id: domain_id.to_string(),
        owner,
        app_type: app_type.to_string(),
        app_name: app_name.to_string(),
    })
}

/// A Studio app created by `CreateApp`, as `DescribeApp` replays it.
///
/// `in_service_at` is the instant the clock owes the `Pending` -> `InService`
/// transition; it is stored, never slept on, and materialised by whichever reporting
/// call first runs at or after it. `deletion_requested_at` carries the same fact for the
/// teardown: the instant the `DeleteApp` that asked for it ran, from which the clock owes
/// the `Deleted` status ([`APP_DELETED_AFTER`]) and, later, the record's removal
/// ([`APP_RETAINED_AFTER`]).
#[derive(Debug, Clone)]
pub struct App {
    pub arn: String,
    pub status: String,
    pub resource_spec: Option<ResourceSpec>,
    pub recovery_mode: Option<bool>,
    pub tags: Vec<Tag>,
    pub creation_time: SystemTime,
    pub in_service_at: SystemTime,
    /// When a `DeleteApp` asked for this app's teardown, or `None` while none has. See
    /// [`App::begin_deletion`].
    pub deletion_requested_at: Option<SystemTime>,
}

impl App {
    /// Materialise the transition the clock already owes — `Pending` -> `InService` for an
    /// app that is launching, `Deleting` -> `Deleted` for one being torn down — the way
    /// [`Endpoint::settle_status`] does for an endpoint.
    ///
    /// Answers whether the transition happened on this call, so the caller can record it.
    pub fn settle_status(&mut self, now: SystemTime) -> bool {
        if let Some(requested) = self.deletion_requested_at {
            if self.status == APP_STATUS_DELETING && now >= requested + APP_DELETED_AFTER {
                self.status = APP_STATUS_DELETED.to_string();
                return true;
            }
            return false;
        }
        if self.status == APP_STATUS_PENDING && now >= self.in_service_at {
            self.status = APP_STATUS_IN_SERVICE.to_string();
            return true;
        }
        false
    }

    /// Start the teardown `DeleteApp` asked for: the app reports `Deleting`, and the clock
    /// owes it `Deleted` [`APP_DELETED_AFTER`] later.
    ///
    /// The record is NOT dropped here: `AppStatus` carries `Deleting` and `Deleted` and
    /// SageMaker answers for a deleted app while it is retained, so both statuses are
    /// observable and the removal is the clock's — stored as an instant and never slept on.
    pub fn begin_deletion(&mut self, now: SystemTime) {
        self.status = APP_STATUS_DELETING.to_string();
        self.deletion_requested_at = Some(now);
    }

    /// Whether a `DeleteApp` has asked for this app's teardown, i.e. the app no longer
    /// runs: it is `Deleting` or has settled on `Deleted`.
    ///
    /// What everything weighing the apps a resource still HOLDS asks — the domain a
    /// teardown would take with it, the image an app runs on, the account's app quota — so
    /// a retained record of a deleted app stands in nothing's way.
    pub fn is_released(&self) -> bool {
        self.deletion_requested_at.is_some()
    }

    /// Whether this app's record has left the retention window, i.e. every reader answers
    /// for the key as one nothing ever created.
    ///
    /// The record itself is dropped by
    /// [`ComAmazonawsSagemakerSagemakerState::release_purged_apps`]; past this instant the
    /// app is gone whether or not that sweep has run yet.
    pub fn is_purged(&self, now: SystemTime) -> bool {
        self.deletion_requested_at
            .is_some_and(|requested| now >= requested + APP_RETAINED_AFTER)
    }

    /// Whether a `CreateApp` for this app's key may launch a new app over it, i.e. the app
    /// has reached one of [`APP_RESTARTABLE_STATUSES`].
    pub fn is_restartable(&self) -> bool {
        APP_RESTARTABLE_STATUSES.contains(&self.status.as_str())
    }
}

impl ComAmazonawsSagemakerSagemakerState {
    /// Whether another app fits, i.e. the account runs fewer than [`APP_QUOTA`] apps.
    ///
    /// The retained records of deleted apps are not counted: they are a reporting window,
    /// not running apps, so a delete frees the slot it held.
    pub fn apps_below_quota(&self) -> bool {
        self.running_app_count() < APP_QUOTA
    }

    /// How many apps the account is running, i.e. the stored apps no `DeleteApp` has torn
    /// down ([`App::is_released`]).
    pub fn running_app_count(&self) -> usize {
        self.apps.values().filter(|app| !app.is_released()).count()
    }

    /// Whether the resources the app `key` hangs off still resolve, i.e. a reader would
    /// answer for them.
    ///
    /// An app lives in a domain and runs for that domain's user profile, so it cannot
    /// outlive either: past the instant the clock completed the removal of its domain
    /// ([`Domain::is_released`]) or of its owning profile ([`UserProfile::is_released`]) the
    /// app is gone too, whether or not the sweeps have dropped the records yet. A space app's
    /// owner is a space, which this service holds no records of, so only its domain is
    /// weighed.
    pub fn app_parents_resolve(&self, key: &AppKey, now: SystemTime) -> bool {
        if !self.domain_resolves(&key.domain_id, now) {
            return false;
        }
        match &key.owner {
            AppOwner::UserProfile(user_profile_name) => {
                self.holds_user_profile(&key.domain_id, user_profile_name, now)
            }
            AppOwner::Space(_) => true,
        }
    }

    /// The app a key names, with the transition the clock owed it materialised, or `None`
    /// when nothing is stored under that key, its retained record has left the window, or
    /// the domain or profile it hangs off is gone.
    ///
    /// The one lookup every app reader and the delete resolve a key with, so all of them
    /// agree on which keys answer and on the status they answer with. The second value is
    /// whether the transition happened on this call, so the caller can record it.
    pub fn app_mut(&mut self, key: &AppKey, now: SystemTime) -> Option<(&mut App, bool)> {
        if !self.app_parents_resolve(key, now) {
            return None;
        }
        let app = self.apps.get_mut(key).filter(|app| !app.is_purged(now))?;
        let settled = app.settle_status(now);
        Some((app, settled))
    }

    /// Drop every deleted app whose record has left the retention window.
    ///
    /// Materialises only what was already due: the removal was owed at the stored instant
    /// whether or not anybody called, and a lookup already answers `None` for such an app
    /// ([`App::is_purged`]), so this changes nothing a later caller sees. The call that
    /// asked for each teardown already recorded it, so nothing is answered back.
    pub fn release_purged_apps(&mut self, now: SystemTime) {
        self.apps.retain(|_, app| !app.is_purged(now));
    }

    /// Drop the retained records of the apps of the domains `domain_ids`, i.e. the apps of
    /// domains whose removal the clock already completed.
    ///
    /// Materialises only what was already due: a domain is only released once nothing it
    /// holds is still running ([`Self::domain_holds_apps`]), so what goes here is the
    /// reporting window of apps already torn down — and that window cannot outlive the
    /// domain whose id addresses them. Called from [`Self::release_deleted_domains`], where
    /// the domains themselves go.
    pub fn release_domain_apps(&mut self, domain_ids: &[String]) {
        if domain_ids.is_empty() {
            return;
        }
        self.apps
            .retain(|key, _| !domain_ids.contains(&key.domain_id));
    }

    /// Drop the apps of the user profiles `owners` — each a `(DomainId, UserProfileName)`
    /// pair — i.e. the apps of profiles whose removal the clock already completed.
    ///
    /// Materialises only what was already due, the way [`Self::release_domain_apps`] does
    /// for a gone domain: an app runs for its profile and on the volume that profile owns
    /// ("when a user profile is deleted, the user loses access to their EFS volume"), so an
    /// app cannot outlive the profile it ran for — a released profile takes its apps, and
    /// the reporting window of the ones already torn down, with it. The owner is matched
    /// case-insensitively because `UserProfileName` "is not case sensitive".
    pub fn release_owner_apps(&mut self, owners: &[(String, String)]) {
        if owners.is_empty() {
            return;
        }
        self.apps.retain(|key, _| {
            let AppOwner::UserProfile(owner) = &key.owner else {
                return true;
            };
            !owners.iter().any(|(domain_id, user_profile_name)| {
                *domain_id == key.domain_id && user_profile_name.eq_ignore_ascii_case(owner)
            })
        });
    }

    /// Keep the retained records of deleted apps inside [`DELETED_APP_RECORDS_CAP`],
    /// dropping the ones deleted longest ago first.
    ///
    /// The retention window is a day, so without this bound a caller creating and deleting
    /// app after app would grow the store without end before any record fell due. Only
    /// records whose app is already torn down are candidates, so no running app is ever
    /// dropped.
    pub fn prune_deleted_apps(&mut self) {
        let mut deleted: Vec<(SystemTime, AppKey)> = self
            .apps
            .iter()
            .filter_map(|(key, app)| {
                app.deletion_requested_at
                    .map(|requested| (requested, key.clone()))
            })
            .collect();
        if deleted.len() <= DELETED_APP_RECORDS_CAP {
            return;
        }
        deleted.sort();
        let excess = deleted.len() - DELETED_APP_RECORDS_CAP;
        for (_, key) in deleted.into_iter().take(excess) {
            self.apps.remove(&key);
        }
    }
}

/// An app's ARN: `...:app/<DomainId>/<owner>/<AppType>/<AppName>`.
///
/// A Studio app is identified by five members, so its ARN names all of them — a single
/// `AppName` would not distinguish two apps of different types under one user profile.
/// Each component is carried exactly as the request supplied it, which is safe for a
/// positional ARN segment because every one of them is already refused at the boundary
/// unless it matches its modelled constraint: `AppName`, `UserProfileName` and
/// `SpaceName` are `len 0..63, pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$`, `DomainId`
/// is `len 0..63, pattern ^d-(-*[a-z0-9]){1,61}$` and `AppType` is an enum
/// (`smithy_query(query="operation", operation="CreateApp")`), so none can carry a `:`,
/// a `/` or a control character and the result always matches `AppArn`'s
/// `^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:app/`. The real service composes
/// the ARN from the same five members, so its `len 0..256` bound binds the same inputs.
pub fn app_arn(region: &str, key: &AppKey) -> String {
    resource_arn(
        region,
        "app",
        &format!(
            "{}/{}/{}/{}",
            key.domain_id,
            key.owner.name(),
            key.app_type,
            key.app_name
        ),
    )
}
