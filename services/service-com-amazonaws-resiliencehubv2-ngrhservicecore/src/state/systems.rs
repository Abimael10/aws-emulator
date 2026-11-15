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
//! [`SystemRecord::to_system`], so `CreateSystem` and `GetSystem` cannot drift from one
//! another or from what was stored.

use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use crate::ops::types::System;

use super::{
    tag_map_view, to_date_time, ComAmazonawsResiliencehubv2NgrhservicecoreState,
    ARN_SERVICE_NAMESPACE,
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
    /// Stored ordered so a tag map reads back the same way every time.
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// Creation order, from the store's monotonic counter — the order a listing scans
    /// in, and the position a page token names.
    pub sequence: u64,
}

impl SystemRecord {
    /// This record as the `System` shape `CreateSystem` / `GetSystem` return.
    ///
    /// `organizationId` and `ouId` are left absent: nothing in this crate joins an
    /// account to an organization or an organizational unit, so there is none to name —
    /// and both members are "displayed only if caller has access".
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
            ou_id: None,
            created_at: Some(to_date_time(self.created_at)),
            updated_at: Some(to_date_time(self.updated_at)),
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
