// SPDX-License-Identifier: CC0-1.0

//! One gateway's rate limits: the record a rate limit is stored as, the
//! clock-owed status it reports, the response envelopes every operation that
//! reports one renders, and the one check a submitted set has to pass.
//!
//! One concern, kept beside the gateway record that holds the limits rather than
//! in any one `ops/` file: a rate limit is addressed only THROUGH its gateway
//! (`/gateways/{gatewayIdentifier}/rate-limits/{rateLimitId}`), so the batch
//! write and every later read answer from this one shape and cannot drift.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use crate::ops::types::{
    BatchPutLimitEntry, CreateGatewayRateLimitOutput, DeleteGatewayRateLimitOutput,
    GatewayRateLimitDetail, GetGatewayRateLimitOutput, LimitEntry, ResourceNotFoundException,
    UpdateGatewayRateLimitOutput, ValidationException,
};

use super::{
    field_validation_failed, gateway_status_conflict, timestamp, GatewayRecord,
    VALIDATION_REASON_RESOURCE_CONFLICT,
};

/// `GatewayRateLimitStatus` members this service drives. The enum's full value
/// list is `CREATING | ACTIVE | UPDATING | DELETING`
/// (`smithy_query shape=GatewayRateLimitStatus`).
pub const GATEWAY_RATE_LIMIT_STATUS_CREATING: &str = "CREATING";
pub const GATEWAY_RATE_LIMIT_STATUS_UPDATING: &str = "UPDATING";
pub const GATEWAY_RATE_LIMIT_STATUS_ACTIVE: &str = "ACTIVE";
pub const GATEWAY_RATE_LIMIT_STATUS_DELETING: &str = "DELETING";

/// How long a rate limit one asynchronous write accepted stays in its transient
/// status (`CREATING` for one a create added, `UPDATING` for one an update
/// replaced) before the clock owes it `ACTIVE`.
///
/// The atomic batch write waits out no span at all: see [`GatewayRateLimitRecord::stored`].
///
/// A rate limit is a child of the gateway that enforces it, so it settles on the
/// same span the gateway family uses ([`super::GATEWAY_SETTLE_DELAY`]): one
/// chosen span for the whole family rather than a second one only this resource
/// would wait out.
pub const GATEWAY_RATE_LIMIT_SETTLE_DELAY: Duration = super::GATEWAY_SETTLE_DELAY;

/// The modelled not-found error for a rate limit no id of a gateway resolves to:
/// the resource, then the id the request's path carried ("Rate limit not found:
/// <rateLimitId>").
///
/// A rate limit is addressed by an id OF its gateway
/// (`/gateways/{gatewayIdentifier}/rate-limits/{rateLimitId}`), and that id is
/// the one thing that tells a missing limit from a missing gateway, so the
/// refusal names it rather than taking the gateway plane's identifier-free
/// sentence ([`super::resource_not_found`]). The text around the id is fixed.
///
/// Shared by every operation that resolves one, so a read and a write cannot
/// report the same missing limit in two sentences.
pub fn gateway_rate_limit_not_found(rate_limit_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("Rate limit not found: {rate_limit_id}")),
    }
}

/// How many rate limits one gateway will hold.
///
/// `BatchPutLimitEntries` is `@length 1..50` and the batch write REPLACES the
/// gateway's whole set, so a submitted set cannot exceed this on its own; the
/// ceiling is named here so every writer of the set is bounded by one number
/// rather than by the shape of its own request.
pub const MAX_RATE_LIMITS_PER_GATEWAY: usize = 50;

/// The request member a refused batch names.
const RATE_LIMITS_MEMBER: &str = "rateLimits";

/// The request member a refused rule entry names, on every request that carries
/// one rate limit's rule entries.
const ENTRIES_MEMBER: &str = "entries";

/// One rate limit configured on a gateway: what the caller submitted for it, and
/// the transition to `ACTIVE` the clock owes it.
///
/// Every operation that reports a rate limit renders it from this record, so a
/// write and a later read cannot disagree about its entries or its status.
#[derive(Debug, Clone)]
pub struct GatewayRateLimitRecord {
    pub rate_limit_id: String,
    pub description: Option<String>,
    pub dimension_keys: Vec<String>,
    pub entries: Vec<LimitEntry>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The status until [`settles_at`](Self::settles_at) passes.
    pub transient_status: String,
    /// The status from [`settles_at`](Self::settles_at) onwards.
    pub settled_status: String,
    /// The instant the transient status is owed its transition. Stored, never
    /// slept on: a read materialises the transition the clock already made due.
    pub settles_at: SystemTime,
    /// The instant this rate limit's accepted deletion is owed its removal from
    /// the gateway that held it, or `None` while no deletion has been accepted.
    /// Stored, never slept on, and materialised by
    /// [`super::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::expire_deleted_gateway_rate_limits`].
    pub removal_due_at: Option<SystemTime>,
}

impl GatewayRateLimitRecord {
    /// The record a write stores for one submitted entry, under the id the write
    /// resolved for it.
    ///
    /// `replacing` is the rate limit that id already named, if any: an upsert
    /// keeps the instant that limit was first created, while an id nothing held
    /// is created at `now`.
    ///
    /// The set is written ATOMICALLY — "Atomically creates or updates multiple
    /// rate limits for a gateway … If the operation fails, the service applies no
    /// changes" (`smithy_query doc shape=BatchPutGatewayRateLimits`) — so the set
    /// this stores is the set already in force: every entry of it is reported in
    /// the terminal `ACTIVE` from the instant of the write, with no transition
    /// left for the clock to owe it. A caller that waits for `ACTIVE` before
    /// sending traffic is served by the batch write itself.
    pub fn stored(
        rate_limit_id: String,
        submitted: BatchPutLimitEntry,
        replacing: Option<&Self>,
        now: SystemTime,
    ) -> Self {
        Self::recorded(
            rate_limit_id,
            submitted.description,
            submitted.dimension_keys,
            submitted.entries,
            replacing,
            now,
            // Nothing is owed: the status is the terminal one from this instant on.
            (GATEWAY_RATE_LIMIT_STATUS_ACTIVE.to_string(), now),
        )
    }

    /// The record one ASYNCHRONOUS write stores, from the members that write
    /// carries rather than from the batch write's own entry shape: the single
    /// create submits one rate limit member by member and the update rewrites
    /// one, and both report the transient status the clock owes a transition out
    /// of — `CREATING` for an id nothing held, `UPDATING` for one being
    /// replaced — until [`GATEWAY_RATE_LIMIT_SETTLE_DELAY`] passes.
    pub fn written(
        rate_limit_id: String,
        description: Option<String>,
        dimension_keys: Vec<String>,
        entries: Vec<LimitEntry>,
        replacing: Option<&Self>,
        now: SystemTime,
    ) -> Self {
        let transient_status = if replacing.is_some() {
            GATEWAY_RATE_LIMIT_STATUS_UPDATING.to_string()
        } else {
            GATEWAY_RATE_LIMIT_STATUS_CREATING.to_string()
        };
        Self::recorded(
            rate_limit_id,
            description,
            dimension_keys,
            entries,
            replacing,
            now,
            (transient_status, now + GATEWAY_RATE_LIMIT_SETTLE_DELAY),
        )
    }

    /// The one place a rate limit's record is built: what the caller submitted,
    /// and `reports` — the status it reports until the instant beside it, from
    /// which the terminal `ACTIVE` is reported instead. Every write goes through
    /// here, so none of them can date a rate limit differently.
    fn recorded(
        rate_limit_id: String,
        description: Option<String>,
        dimension_keys: Vec<String>,
        entries: Vec<LimitEntry>,
        replacing: Option<&Self>,
        now: SystemTime,
        reports: (String, SystemTime),
    ) -> Self {
        let (transient_status, settles_at) = reports;
        Self {
            rate_limit_id,
            description,
            dimension_keys,
            entries,
            created_at: replacing.map_or(now, |previous| previous.created_at),
            updated_at: now,
            transient_status,
            settled_status: GATEWAY_RATE_LIMIT_STATUS_ACTIVE.to_string(),
            settles_at,
            // A stored rate limit is one nobody has asked to delete: a write
            // records a configuration, never a removal.
            removal_due_at: None,
        }
    }

    /// Whether this rate limit's deletion has been accepted, so it is on its way
    /// out and reports `DELETING` until the clock owes its removal.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// Accept this rate limit's deletion at `now`: it reports `DELETING` from
    /// this instant and is removed from the gateway that held it once
    /// [`GATEWAY_RATE_LIMIT_SETTLE_DELAY`] passes, so a caller that deletes and
    /// then polls sees `DELETING` and then the modelled 404 rather than a limit
    /// that vanished mid-call.
    ///
    /// `DeleteGatewayRateLimit` is `@idempotent`
    /// (`smithy_query(query="operation",operation="DeleteGatewayRateLimit")`), so a
    /// repeat while the removal is still owed reports that same limit instead of
    /// re-arming the clock.
    pub fn accept_deletion(&mut self, now: SystemTime) {
        if self.is_deleting() {
            return;
        }
        self.transient_status = GATEWAY_RATE_LIMIT_STATUS_DELETING.to_string();
        self.settled_status = GATEWAY_RATE_LIMIT_STATUS_DELETING.to_string();
        self.settles_at = now;
        self.removal_due_at = Some(now + GATEWAY_RATE_LIMIT_SETTLE_DELAY);
        self.updated_at = now;
    }

    /// The status this rate limit reports at `now`: the transient one until the
    /// transition the clock owes comes due, the settled one afterwards.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// This rate limit as the `GatewayRateLimitDetail` every list- and
    /// batch-shaped response reports, under the gateway identifier the request
    /// addressed it through.
    pub fn detail(&self, gateway_identifier: &str, now: SystemTime) -> GatewayRateLimitDetail {
        GatewayRateLimitDetail {
            rate_limit_id: self.rate_limit_id.clone(),
            gateway_identifier: gateway_identifier.to_string(),
            description: self.description.clone(),
            dimension_keys: self.dimension_keys.clone(),
            entries: self.entries.clone(),
            status: self.status_at(now).to_string(),
            created_at: timestamp(self.created_at),
            updated_at: timestamp(self.updated_at),
        }
    }

    /// This rate limit as `GetGatewayRateLimit`'s response envelope — the same
    /// members [`Self::detail`] renders, read off the same record, so a read
    /// cannot differ from the write that stored it.
    pub fn get_output(
        &self,
        gateway_identifier: &str,
        now: SystemTime,
    ) -> GetGatewayRateLimitOutput {
        let detail = self.detail(gateway_identifier, now);
        GetGatewayRateLimitOutput {
            rate_limit_id: detail.rate_limit_id,
            gateway_identifier: detail.gateway_identifier,
            description: detail.description,
            dimension_keys: detail.dimension_keys,
            entries: detail.entries,
            status: detail.status,
            created_at: detail.created_at,
            updated_at: detail.updated_at,
        }
    }

    /// This rate limit as `CreateGatewayRateLimit`'s response envelope, which
    /// declares the same members [`Self::get_output`] does, read off the same
    /// record: what the create reports is what the next read reports.
    pub fn create_output(
        &self,
        gateway_identifier: &str,
        now: SystemTime,
    ) -> CreateGatewayRateLimitOutput {
        let detail = self.detail(gateway_identifier, now);
        CreateGatewayRateLimitOutput {
            rate_limit_id: detail.rate_limit_id,
            gateway_identifier: detail.gateway_identifier,
            description: detail.description,
            dimension_keys: detail.dimension_keys,
            entries: detail.entries,
            status: detail.status,
            created_at: detail.created_at,
            updated_at: detail.updated_at,
        }
    }

    /// This rate limit as `UpdateGatewayRateLimit`'s response envelope, which
    /// declares the same members [`Self::get_output`] does, read off the same
    /// record: what the update reports is what the next read reports.
    pub fn update_output(
        &self,
        gateway_identifier: &str,
        now: SystemTime,
    ) -> UpdateGatewayRateLimitOutput {
        let detail = self.detail(gateway_identifier, now);
        UpdateGatewayRateLimitOutput {
            rate_limit_id: detail.rate_limit_id,
            gateway_identifier: detail.gateway_identifier,
            description: detail.description,
            dimension_keys: detail.dimension_keys,
            entries: detail.entries,
            status: detail.status,
            created_at: detail.created_at,
            updated_at: detail.updated_at,
        }
    }

    /// This rate limit as `DeleteGatewayRateLimit`'s response envelope: the id
    /// the request addressed it by and the status it reports from this instant,
    /// read off the same record every other response of it is read off.
    pub fn delete_output(&self, now: SystemTime) -> DeleteGatewayRateLimitOutput {
        DeleteGatewayRateLimitOutput {
            rate_limit_id: self.rate_limit_id.clone(),
            status: self.status_at(now).to_string(),
        }
    }
}

/// The modelled validation error a write of a rate limit gets while the limit it
/// addresses is in a status that cannot take it: the operation that was asked
/// for, the limit it named and the status it found.
///
/// A rate limit whose deletion has been accepted is on its way out on the clock
/// and takes no further configuration, so a write of it is refused rather than
/// cancelling the removal the clock already owes. The sentence is fixed and its
/// variable parts are the asking operation's name, the `rateLimitId` the request
/// carried and the status token the limit's enum declares
/// (`smithy_query(query="shape",shape="GatewayRateLimitStatus")`) — the same form the
/// response members carry it in.
///
/// `reason` is [`VALIDATION_REASON_RESOURCE_CONFLICT`] and there is no
/// `fieldList`, for the reason [`gateway_status_conflict`] has none: nothing is
/// wrong with the request's members, it is the limit's state that refuses it.
pub fn rate_limit_status_conflict(
    operation: &str,
    rate_limit_id: &str,
    status: &str,
) -> ValidationException {
    ValidationException {
        message: format!(
            "Cannot perform operation {operation} when rate limit {rate_limit_id} is in {status} \
             status"
        ),
        reason: VALIDATION_REASON_RESOURCE_CONFLICT.to_string(),
        field_list: None,
    }
}

/// The refusal a write of a STORED rate limit owes when that limit is already on
/// its way out, or `None` when it can still take the write.
///
/// Shared by every operation that reconfigures one limit in place, so a deletion
/// the clock owes cannot be cancelled by one of them.
pub fn refusal_for_rate_limit(
    operation: &str,
    record: &GatewayRateLimitRecord,
    now: SystemTime,
) -> Option<ValidationException> {
    record.is_deleting().then(|| {
        rate_limit_status_conflict(operation, &record.rate_limit_id, record.status_at(now))
    })
}

/// The refusal a write of ONE rate limit owes when a submitted rule entry names
/// a dimension that limit is not scoped by, or `None` when every entry is in
/// scope.
///
/// The rule [`undeclared_dimension`] decides, worded once here so every write of
/// a single rate limit refuses it in the same sentence naming the same member:
/// the create, which is scoped by the `dimensionKeys` the request carries, and
/// the update, which is scoped by the keys the stored limit was created with
/// because they are immutable after creation.
pub fn refusal_for_entries(
    dimension_keys: &[String],
    entries: &[LimitEntry],
) -> Option<ValidationException> {
    let declared: BTreeSet<&str> = dimension_keys.iter().map(String::as_str).collect();
    undeclared_dimension(&declared, entries).map(|unknown| {
        field_validation_failed(
            ENTRIES_MEMBER,
            &format!(
                "names the dimension {unknown} in a rule entry; a rule entry's dimensions are \
                 the rate limit's own dimension keys {}",
                dimension_keys.join(", ")
            ),
        )
    })
}

/// The dimension a rule entry names that a rate limit scoped by `declared` does
/// not itself declare, if any.
///
/// The rule the model states in prose, so the boundary constraint walk cannot
/// refuse it: "Keys must match the parent rate limit's dimension keys"
/// (`smithy_query doc shape=LimitEntry member=dimensions`). Every write of a
/// rate limit applies it — the batch write over each submitted entry, the single
/// create over the one it stores — so the two cannot disagree about which rule
/// entries a rate limit can hold.
pub fn undeclared_dimension<'a>(
    declared: &BTreeSet<&str>,
    entries: &'a [LimitEntry],
) -> Option<&'a str> {
    entries
        .iter()
        .flat_map(|rule| rule.dimensions.keys())
        .map(String::as_str)
        .find(|dimension| !declared.contains(dimension))
}

/// The rate limit already held by this gateway that is scoped by exactly the
/// dimension keys `dimension_keys`, if one is.
///
/// "Must be unique per gateway—no two rate limits can share the same dimension
/// keys" (`smithy_query doc shape=CreateGatewayRateLimitRequest
/// member=dimensionKeys`): the keys are compared as a SET, for the reason
/// [`refusal_for_batch`] compares them that way — the list's order decides how a
/// wildcard may be used inside one rate limit, while two limits scoped by the
/// same keys in a different order still share those keys.
pub fn rate_limit_sharing_dimension_keys<'a>(
    rate_limits: &'a BTreeMap<String, GatewayRateLimitRecord>,
    dimension_keys: &[String],
) -> Option<&'a GatewayRateLimitRecord> {
    let submitted: BTreeSet<&str> = dimension_keys.iter().map(String::as_str).collect();
    rate_limits.values().find(|held| {
        held.dimension_keys
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<&str>>()
            == submitted
    })
}

/// The refusal a write of one gateway's configuration owes when the gateway it
/// addresses cannot take it, or `None` when it can.
///
/// A gateway whose deletion has been accepted is on its way out on the clock and
/// takes no further configuration, so a write of its rate limits or its rules is
/// refused with the status the caller has to wait out — worded by the one helper
/// every gateway operation this service refuses on status shares.
///
/// The TRANSIENT status the clock owes a gateway a transition OUT of (`CREATING`
/// after a create, `UPDATING` after an update) is NOT a refusal: a rate limit and
/// a rule each carry a clock-owed status of their own, so a caller configures a
/// gateway as soon as the gateway exists rather than waiting it out, which is
/// what the service these operations emulate does.
///
/// Shared by every write of a gateway's child configuration — the single
/// rate-limit create, the batch write and the rule create — so they cannot put
/// different preconditions on the same gateway.
pub fn refusal_for_gateway(
    operation: &str,
    gateway: &GatewayRecord,
    now: SystemTime,
) -> Option<ValidationException> {
    gateway
        .is_deleting()
        .then(|| gateway_status_conflict(operation, gateway.status_at(now)))
}

/// Refuse a submitted set that cannot BE one gateway's rate-limit set, with the
/// modelled `ValidationException` naming the member and why.
///
/// Three rules the model states in prose, so the boundary constraint walk cannot
/// refuse them: an id is what the set is keyed by, so one id cannot appear twice
/// in the same submission; "no two rate limits can share the same dimension
/// keys" (`smithy_query doc shape=CreateGatewayRateLimitRequest
/// member=dimensionKeys`); and a rule entry's dimension names are the parent
/// limit's — "Keys must match the parent rate limit's dimension keys"
/// (`smithy_query doc shape=LimitEntry member=dimensions`).
pub fn refusal_for_batch(submitted: &[BatchPutLimitEntry]) -> Option<ValidationException> {
    let mut ids = BTreeSet::new();
    let mut dimension_key_sets = BTreeSet::new();
    for entry in submitted {
        if let Some(rate_limit_id) = &entry.rate_limit_id {
            if !ids.insert(rate_limit_id.clone()) {
                return Some(field_validation_failed(
                    RATE_LIMITS_MEMBER,
                    &format!(
                        "carries the rate limit {rate_limit_id} more than once; a rateLimitId \
                         identifies one entry of the set"
                    ),
                ));
            }
        }
        let keys: Vec<String> = entry.dimension_keys.clone();
        // The dimension keys are compared as a SET: the list's order decides how
        // a wildcard may be used inside one rate limit, but two rate limits
        // scoped by the same keys in a different order still share those keys,
        // which is what the rule forbids.
        let declared: BTreeSet<&str> = entry.dimension_keys.iter().map(String::as_str).collect();
        if !dimension_key_sets.insert(declared.clone()) {
            return Some(field_validation_failed(
                RATE_LIMITS_MEMBER,
                &format!(
                    "carries two rate limits with the dimension keys {}; no two rate limits of a \
                     gateway can share the same dimension keys",
                    keys.join(", ")
                ),
            ));
        }
        if let Some(unknown) = undeclared_dimension(&declared, &entry.entries) {
            return Some(field_validation_failed(
                RATE_LIMITS_MEMBER,
                &format!(
                    "names the dimension {unknown} in a rule entry of the rate limit with the \
                     dimension keys {}; a rule entry's dimensions are the rate limit's own \
                     dimension keys",
                    keys.join(", ")
                ),
            ));
        }
    }
    None
}
