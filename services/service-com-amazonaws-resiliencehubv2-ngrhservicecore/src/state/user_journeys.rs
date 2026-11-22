// SPDX-License-Identifier: CC0-1.0

//! The `UserJourney` side of this service's store.
//!
//! A user journey exists only WITHIN a system — `CreateUserJourney` takes the system's
//! `@required` ARN beside the journey's own members, and every operation that reaches one
//! names that ARN plus the `userJourneyId` — so the records live inside the parent system's
//! record ([`super::SystemRecord::user_journeys`]), the way a service's assertions and
//! service functions live inside its own: removing the system removes its journeys with it,
//! and no lookup can reach a journey whose system is gone.
//!
//! Every `UserJourney` body this crate returns is projected from a record here through
//! [`UserJourneyRecord::to_user_journey`], so the writer and the reader cannot drift from
//! one another or from what was stored.
//!
//! The journeys are also what a service's `associatedSystems` entry REFERENCES: its
//! `userJourneyIds` are "the list of user journey identifiers that associate this system
//! with the service", so an entry is resolved against the journeys of the system it names
//! (see [`super::ComAmazonawsResiliencehubv2NgrhservicecoreState::unresolved_association_message`]).

use std::time::SystemTime;

use crate::ops::types::{ResourceNotFoundException, UserJourney, UserJourneySummary};

use super::{not_found_message, to_date_time, SystemRecord};

/// `resourceType` for a user journey: the model's own shape name for it, the way every
/// other refusal in this crate reports the type of the resource it could not find.
pub const USER_JOURNEY_RESOURCE_TYPE: &str = "UserJourney";

/// How a refusal's MESSAGE names a user journey: the resource as the service words it in
/// prose, which is not its shape name.
pub const USER_JOURNEY_LABEL: &str = "User journey";

/// The ceiling on how many user journeys one system will hold, so an unauthenticated caller
/// cannot grow the process's memory without limit by repeating `CreateUserJourney`.
///
/// CHOSEN, not documented: the model publishes no per-system journey quota, so this mirrors
/// the per-parent ceiling the `Assertion` side of this store uses
/// ([`super::MAX_ASSERTIONS_PER_SERVICE`]), a hundred per parent. `CreateUserJourney` refuses a create that would
/// pass it with the `ServiceQuotaExceededException` (HTTP 402) its model declares. The
/// per-system idempotency index is bounded by the same ceiling: a token already in it either
/// replays the stored journey or is refused, never inserting a second entry.
pub const MAX_USER_JOURNEYS_PER_SYSTEM: usize = super::MAX_ASSERTIONS_PER_SERVICE;

/// One user journey created on a system, and the sole source of every `UserJourney` body
/// this crate returns.
#[derive(Debug, Clone)]
pub struct UserJourneyRecord {
    /// The minted `userJourneyId`; also this record's key in
    /// [`super::SystemRecord::user_journeys`].
    pub user_journey_id: String,
    pub name: String,
    pub description: Option<String>,
    /// The resilience policy this journey is held to, as the request named it.
    pub policy_arn: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// Creation order, from the store's monotonic counter — the order a listing scans in.
    pub sequence: u64,
}

impl UserJourneyRecord {
    /// This record as the `UserJourney` shape `CreateUserJourney` and `GetUserJourney`
    /// return.
    pub fn to_user_journey(&self) -> UserJourney {
        UserJourney {
            user_journey_id: self.user_journey_id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            policy_arn: self.policy_arn.clone(),
            created_at: Some(to_date_time(self.created_at)),
            updated_at: Some(to_date_time(self.updated_at)),
        }
    }

    /// This record as the `UserJourneySummary` shape `ListUserJourneys` emits.
    ///
    /// `UserJourneySummary` declares only `userJourneyId`, `name`, `createdAt` and
    /// `updatedAt`, so a summary reports those four from the SAME record
    /// [`Self::to_user_journey`] projects — the listing and the read cannot drift — and
    /// reports nothing the summary shape does not declare.
    pub fn to_summary(&self) -> UserJourneySummary {
        UserJourneySummary {
            user_journey_id: self.user_journey_id.clone(),
            name: self.name.clone(),
            created_at: Some(to_date_time(self.created_at)),
            updated_at: Some(to_date_time(self.updated_at)),
        }
    }
}

/// The creation parameters one `CreateUserJourney` call carried, as a `clientToken`
/// remembers them.
///
/// Everything the request says about the journey to create, and nothing the service derives
/// (the id, the timestamps) or the system it was created under (the token is remembered
/// inside that system's record). Two requests are the SAME create when these compare equal,
/// which is what decides whether a repeated token replays the first call or is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserJourneyCreateRequest {
    pub name: String,
    pub description: Option<String>,
    pub policy_arn: Option<String>,
}

/// What a consumed `CreateUserJourney` `clientToken` remembers: which journey it created,
/// and the request it created it for.
#[derive(Debug, Clone)]
pub struct UserJourneyIdempotencyRecord {
    pub user_journey_id: String,
    pub request: UserJourneyCreateRequest,
}

impl SystemRecord {
    /// This system's user journeys in creation order — what a listing enumerates before it
    /// projects them.
    pub fn user_journeys_in_creation_order(&self) -> Vec<&UserJourneyRecord> {
        let mut journeys: Vec<&UserJourneyRecord> = self.user_journeys.values().collect();
        journeys.sort_by_key(|journey| journey.sequence);
        journeys
    }
}

/// A fresh `userJourneyId`.
///
/// `UserJourneyId` is constrained `len 1..255` / `^\S{1,255}$`, which the UUID this crate's
/// one minting path produces satisfies; none of its bits comes from the request, so two
/// journeys created under the same name never collide.
pub fn mint_user_journey_id(region: &str) -> String {
    super::mint_uuid(region)
}

/// The refusal for a `userJourneyId` that names no stored user journey of its system.
///
/// A journey is identified by that id within its system rather than by an ARN of its own, so
/// the id is both the `resourceId` and what the message names — the same shape the refusal
/// for a service function takes. The message names the resource in prose
/// ([`USER_JOURNEY_LABEL`]) while `resourceType` carries the model's own shape name.
pub fn user_journey_not_found(user_journey_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: not_found_message(USER_JOURNEY_LABEL, user_journey_id),
        resource_id: Some(user_journey_id.to_string()),
        resource_type: Some(USER_JOURNEY_RESOURCE_TYPE.to_string()),
    }
}

/// The sentence a request REFERENCING a user journey that its system does not hold is
/// refused with.
///
/// A journey id is only meaningful within one system, so the refusal names both: the id the
/// request supplied and the system it was looked for in.
pub fn user_journey_not_found_for_system_message(user_journey_id: &str, system_arn: &str) -> String {
    format!(
        "{} for system: {system_arn}",
        not_found_message(USER_JOURNEY_LABEL, user_journey_id)
    )
}
