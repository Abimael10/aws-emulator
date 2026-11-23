// SPDX-License-Identifier: CC0-1.0

//! The `System` side of this service's store.
//!
//! One [`SystemRecord`] per created system, keyed by its ARN in
//! [`super::ComAmazonawsResiliencehubv2NgrhservicecoreState::systems`], plus the two
//! secondary indexes the system operations need — a name index for the duplicate-name
//! `ConflictException`, and a `clientToken` index for idempotent replay — the same shape
//! the `Service` and `Policy` sides of the store use.
//!
//! The `clientToken` index differs from theirs in WHAT it stores: a
//! [`SystemIdempotencyRecord`] keeps the creation parameters the token was first used
//! with beside the ARN it created, because `CreateSystem` replays a token only for the
//! request it already served and refuses the same token carrying different parameters
//! (see [`SystemCreateRequest`]).
//!
//! Every `System` body this crate returns is projected from a record here through
//! [`SystemRecord::to_system`], and every `SystemSummary` through
//! [`SystemRecord::to_summary`], which reads that same projection — so `CreateSystem`,
//! `GetSystem` and `ListSystems` cannot drift from one another or from what was stored.

use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use crate::ops::types::{AssociatedSystem, System, SystemSummary};

use super::{
    not_found_message, tag_map_view, to_date_time, user_journey_not_found_for_system_message,
    ComAmazonawsResiliencehubv2NgrhservicecoreState, UserJourneyIdempotencyRecord,
    UserJourneyRecord, ARN_SERVICE_NAMESPACE, SYSTEM_RESOURCE_TYPE,
};

/// The resource type a system's ARN carries.
///
/// From the model's own `IamSystemResource`, whose `@iamResource` name is `system`: the
/// resource segment of an ARN is that resource name, the way a service's is `service`.
pub const SYSTEM_RESOURCE: &str = "system";

/// The ceiling on how many systems this store will hold, so an unauthenticated caller
/// cannot grow the process's memory without limit by repeating `CreateSystem`.
///
/// CHOSEN, not documented: the model publishes no system quota, so this mirrors the
/// ceiling the `Service` side of this store uses ([`super::MAX_SERVICES`]).
/// `CreateSystem` refuses a create that would pass it with the
/// `ServiceQuotaExceededException` (HTTP 402) its model declares. The two secondary
/// indexes are bounded by the same ceiling: `system_names` holds exactly one entry per
/// stored system, and `system_idempotency` at most one (a token already in it either
/// replays the stored system or is refused, never inserting a second entry).
pub const MAX_SYSTEMS: usize = super::MAX_SERVICES;

/// One created system, and the sole source of every `System` body this crate returns.
#[derive(Debug, Clone)]
pub struct SystemRecord {
    /// The minted ARN; also this record's key in
    /// [`super::ComAmazonawsResiliencehubv2NgrhservicecoreState::systems`].
    pub system_arn: String,
    /// The minted `systemId` — the resource id the ARN carries after `system/`, so the
    /// two identifiers of one system always name the same resource.
    pub system_id: String,
    pub name: String,
    pub description: Option<String>,
    /// Whether cross-account sharing is enabled for this system. A system created
    /// without the member has sharing off, so this is the flag's stored state rather
    /// than a member the projection withholds — the same reading the `Policy` side of
    /// this store takes of its own `sharingEnabled`.
    pub sharing_enabled: bool,
    pub kms_key_id: Option<String>,
    /// The organizational unit this system's account sits in. Nothing in this slice sets
    /// it; `ListSystems`' `ouId` filter compares against it and both system projections
    /// report it — the same reading the `Service` side of this store takes of its own
    /// `ouId` ([`super::ServiceRecord::ou_id`]).
    pub ou_id: Option<String>,
    /// Stored ordered so a tag map reads back the same way every time.
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// Creation order, from the store's monotonic counter — the order a listing scans
    /// in, and the position a page token names.
    pub sequence: u64,
    /// The user journeys created on this system, keyed by `userJourneyId`. See
    /// [`user_journeys`](super::user_journeys): a journey exists only within its system, so
    /// deleting the system takes its journeys with it.
    pub user_journeys: BTreeMap<String, UserJourneyRecord>,
    /// `clientToken` -> what that token created on this system, so a repeat of the
    /// `CreateUserJourney` call it was first used for answers the journey that call created.
    pub user_journey_idempotency: BTreeMap<String, UserJourneyIdempotencyRecord>,
}

impl SystemRecord {
    /// This record as the `System` shape `CreateSystem` / `GetSystem` return.
    ///
    /// `organizationId` is left absent: nothing in this crate joins an account to an
    /// organization, so there is none to name — and the member is "displayed only if
    /// caller has access". `ouId` is the record's stored value, which is likewise absent
    /// until something sets it.
    pub fn to_system(&self) -> System {
        System {
            system_arn: self.system_arn.clone(),
            system_id: self.system_id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            sharing_enabled: Some(self.sharing_enabled),
            tags: tag_map_view(&self.tags),
            kms_key_id: self.kms_key_id.clone(),
            organization_id: None,
            ou_id: self.ou_id.clone(),
            created_at: Some(to_date_time(self.created_at)),
            updated_at: Some(to_date_time(self.updated_at)),
        }
    }

    /// This record as the `SystemSummary` shape `ListSystems` enumerates — the same stored
    /// values the full `System` projection reads, narrowed to the summary's own members.
    ///
    /// `userJourneysCount` is "the number of user journeys in the system": the LENGTH of
    /// the journeys stored on this record, so the count and the journeys it counts can
    /// never disagree. `servicesCount` is "the number of services in the system", which is
    /// counted from the services that reference this system rather than stored here, so it
    /// is passed in by the store that holds both resources (see
    /// [`ComAmazonawsResiliencehubv2NgrhservicecoreState::system_service_count`]) the way
    /// `associatedServiceCount` is passed into the policy projection.
    pub fn to_summary(&self, services_count: i32) -> SystemSummary {
        let system = self.to_system();
        SystemSummary {
            system_id: system.system_id,
            name: system.name,
            system_arn: Some(system.system_arn),
            user_journeys_count: Some(self.user_journeys.len() as i32),
            services_count: Some(services_count),
            organization_id: system.organization_id,
            ou_id: system.ou_id,
            created_at: system.created_at,
            updated_at: system.updated_at,
        }
    }
}

/// The creation parameters one `CreateSystem` call carried, as a `clientToken` remembers
/// them.
///
/// Everything the request says about the system to create, and nothing else: the token
/// itself is not part of it, and neither is anything the service derives (the ARN, the
/// id, the timestamps). Two requests are the SAME create when these compare equal, which
/// is what decides whether a repeated token replays the first call or is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemCreateRequest {
    pub name: String,
    pub description: Option<String>,
    /// Compared as the request SET it, not as it was stored: a request that omits the
    /// flag asks for the service's default, which is not the same request as one that
    /// states a value.
    pub sharing_enabled: Option<bool>,
    pub kms_key_id: Option<String>,
    /// Ordered, so two requests carrying the same tags compare equal whatever order the
    /// map arrived in.
    pub tags: BTreeMap<String, String>,
}

impl SystemCreateRequest {
    /// The creation parameters of a `CreateSystem` request.
    pub fn new(
        name: String,
        description: Option<String>,
        sharing_enabled: Option<bool>,
        kms_key_id: Option<String>,
        tags: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            name,
            description,
            sharing_enabled,
            kms_key_id,
            tags: tags.unwrap_or_default().into_iter().collect(),
        }
    }
}

/// What a consumed `clientToken` remembers: which system it created, and the request it
/// created it for.
#[derive(Debug, Clone)]
pub struct SystemIdempotencyRecord {
    pub system_arn: String,
    pub request: SystemCreateRequest,
}

impl ComAmazonawsResiliencehubv2NgrhservicecoreState {
    /// The `System` body for `system_arn`, or `None` when no system is stored under it.
    ///
    /// The one projection path every system reader answers from.
    pub fn system_view(&self, system_arn: &str) -> Option<System> {
        self.systems.get(system_arn).map(SystemRecord::to_system)
    }

    /// The system stored under `name`, if any — what a create consults before storing a
    /// second system under a name already taken.
    pub fn system_named(&self, name: &str) -> Option<&SystemRecord> {
        let arn = self.system_names.get(name)?;
        self.systems.get(arn)
    }

    /// Every stored system in creation order.
    pub fn systems_in_creation_order(&self) -> Vec<&SystemRecord> {
        let mut records: Vec<&SystemRecord> = self.systems.values().collect();
        records.sort_by_key(|record| record.sequence);
        records
    }

    /// The refusal MESSAGE for the first entry of `associated` that references something
    /// this store does not hold, or `None` when every reference in every entry resolves.
    ///
    /// An `AssociatedSystem` is a REFERENCE, not a value: the shape carries the model's
    /// `@references` entry for the system resource, its `@required` `systemArn` names one of
    /// the account's systems, and its `userJourneyIds` are "the list of user journey
    /// identifiers that associate this system with the service" — identifiers of journeys
    /// OF that system. Both are resolved against the store before an operation writes an
    /// association: the members' patterns are already enforced, and a well-formed ARN or id
    /// is no evidence that what it names exists. The entries are checked in the order the
    /// request sent them, and the system before the journeys it carries, so a reference that
    /// cannot resolve names itself in the refusal.
    pub fn unresolved_association_message(
        &self,
        associated: &[AssociatedSystem],
    ) -> Option<String> {
        for entry in associated {
            let Some(system) = self.systems.get(&entry.system_arn) else {
                return Some(not_found_message(SYSTEM_RESOURCE_TYPE, &entry.system_arn));
            };
            let journeys = entry.user_journey_ids.as_deref().unwrap_or_default();
            if let Some(absent) = journeys
                .iter()
                .find(|journey_id| !system.user_journeys.contains_key(*journey_id))
            {
                return Some(user_journey_not_found_for_system_message(
                    absent,
                    &entry.system_arn,
                ));
            }
        }
        None
    }

    /// How many stored services are associated with `system_arn` — the services IN that
    /// system, which is what `SystemSummary$servicesCount` reports.
    ///
    /// Counted at read time from the services that reference the system, so the count is
    /// the one in force when the call ran rather than a number frozen when the system was
    /// created.
    pub fn system_service_count(&self, system_arn: &str) -> i32 {
        self.services
            .values()
            .filter(|service| {
                service
                    .associated_systems
                    .iter()
                    .any(|entry| entry.system_arn == system_arn)
            })
            .count() as i32
    }

    /// Whether any stored service still lists `system_arn` among its `associatedSystems`.
    ///
    /// What stands between a system and its deletion: a system a service is associated with
    /// is in use, so removing it would leave that service naming a system that is not
    /// there.
    pub fn system_is_associated(&self, system_arn: &str) -> bool {
        self.system_service_count(system_arn) > 0
    }

    /// Remove the system stored under `system_arn`, with the two secondary indexes that
    /// name it, and answer the record that was removed.
    ///
    /// The name is free again for a later create, and no `clientToken` may replay into a
    /// system that no longer exists — the same cleanup a deleted service takes with it.
    pub fn remove_system(&mut self, system_arn: &str) -> Option<SystemRecord> {
        let record = self.systems.remove(system_arn)?;
        self.system_names.remove(&record.name);
        self.system_idempotency
            .retain(|_, consumed| consumed.system_arn != record.system_arn);
        Some(record)
    }
}

/// The refusal MESSAGE for a `DeleteSystem` whose system is still associated with at least
/// one service.
///
/// A system is deletable once no service is associated with it, so the refusal says that is
/// what stands in the way, and names the system it is about by its own `systemId` — fixed
/// text around that one value. The services themselves are not part of it: the caller
/// addressed a system, and which services hold it is what `ListServices(systemArn=…)`
/// answers.
pub fn system_in_use_message(system_id: &str) -> String {
    format!("Cannot delete system with associated services: {system_id}")
}

/// Mint a fresh system id and the ARN that carries it:
/// `arn:aws:resiliencehub:<region>:<account>:system/<uuid>`.
///
/// The account segment is the emulator's canonical one, through `engine`'s
/// [`emulator_arn`](::engine::emulator_arn); the id is a fresh UUID, which satisfies
/// `SystemId`'s modelled `len 1..255` / `^\S{1,255}$`, so two systems never collide even
/// under the same name after a delete. The id is RETURNED beside the ARN rather than
/// discarded, because `System$systemId` reports it on its own.
pub fn mint_system_arn(region: &str) -> (String, String) {
    let system_id = super::mint_uuid(region);
    let system_arn = ::engine::emulator_arn(
        ARN_SERVICE_NAMESPACE,
        region,
        &format!("{SYSTEM_RESOURCE}/{system_id}"),
    );
    (system_arn, system_id)
}
