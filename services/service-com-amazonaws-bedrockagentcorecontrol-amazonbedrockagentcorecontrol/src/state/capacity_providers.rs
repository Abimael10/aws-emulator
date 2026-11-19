// SPDX-License-Identifier: CC0-1.0

//! The capacity provider (`/capacity-providers`): one [`CapacityProviderRecord`] per
//! `capacityProviderId`, holding everything `CreateCapacityProvider` persisted and the
//! status the clock owes it, so the create and every later read of the provider answer
//! from one store.
//!
//! Lives beside the stores in [`super`] rather than in any one `ops/` file, so the create
//! and every read of a capacity provider cannot drift from each other.

use super::*;

use crate::ops::types::{
    CapacityProviderSummary, ComputeConfiguration, CreateCapacityProviderOutput,
    GetCapacityProviderOutput, PermissionsConfiguration,
};

/// The modelled not-found error for a capacity provider this account does not hold.
///
/// This control plane words a missing resource in terms of the id the request named — the
/// sentence around the identifier is fixed, and the identifier itself is the caller's own
/// value — exactly as [`browser_not_found`] words a sibling resource. Shared by the
/// handlers rather than written per operation, so one provider cannot be reported missing in
/// two different sentences by two operations.
pub fn capacity_provider_not_found(capacity_provider_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!(
            "Capacity provider with ID {capacity_provider_id} does not exist"
        )),
    }
}

/// How this control plane spells the capacity provider in a duplicate-name refusal.
const CAPACITY_PROVIDER_CONFLICT_LABEL: &str = "Capacity provider";

/// The modelled conflict for a capacity provider `name` this account already holds.
///
/// `CreateCapacityProvider`'s name "must be unique within your account"
/// (`smithy_query doc shape=CreateCapacityProvider`), and the refusal is this control plane's
/// fixed sentence ([`super::name_conflict`]) around the name the request carried.
pub fn capacity_provider_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(CAPACITY_PROVIDER_CONFLICT_LABEL, name)
}

/// The ARN resource type a capacity provider carries. `CapacityProviderArn` ends in
/// `…:capacity-provider/{capacityProviderId}` (`smithy_query shape=CapacityProviderArn`).
pub const CAPACITY_PROVIDER_ARN_RESOURCE: &str = "capacity-provider";

/// `arn:aws:bedrock-agentcore:{region}:{account}:capacity-provider/{capacityProviderId}` —
/// `CapacityProviderArn`'s form.
pub fn capacity_provider_arn(region: &str, capacity_provider_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("{CAPACITY_PROVIDER_ARN_RESOURCE}/{capacity_provider_id}"),
    )
}

/// `CapacityProviderStatus` members this service drives. The enum's full value list is
/// `CREATING | CREATE_FAILED | UPDATING | UPDATE_FAILED | READY | DELETING | DELETE_FAILED`
/// (`smithy_query shape=CapacityProviderStatus`); the failure statuses belong to a
/// provisioning that failed, which nothing here makes happen, and the documented
/// `statusCode`/`statusReason` that explain one are reported only for such a provider.
pub const CAPACITY_PROVIDER_STATUS_CREATING: &str = "CREATING";
pub const CAPACITY_PROVIDER_STATUS_READY: &str = "READY";

/// How long a freshly created capacity provider stays `CREATING` before the clock owes it
/// `READY`.
///
/// The model documents the transition but no duration for it — "After you create the capacity
/// provider, it enters a CREATING state and transitions to READY when it is available for
/// use" (`smithy_query doc shape=CreateCapacityProvider`) — so the span is chosen: the same
/// one the sibling resources' transitions take ([`super::REGISTRY_SETTLE_DELAY`]), long
/// enough that a caller acting straight after the create still observes the transient
/// status.
pub const CAPACITY_PROVIDER_SETTLE_DELAY: Duration = super::REGISTRY_SETTLE_DELAY;

/// How many capacity providers this emulator will hold.
///
/// `CreateCapacityProvider` models `ServiceQuotaExceededException` (HTTP 402) for exceeding
/// the real service's quota; this number is not a fidelity claim about that quota but the
/// emulator's own ceiling, set generously so no realistic workflow meets it, so a caller
/// looping on create cannot grow this process's memory without bound.
pub const MAX_CAPACITY_PROVIDERS: usize = 1_000;

/// One capacity provider: everything `CreateCapacityProvider` persisted for it, and the
/// status the clock owes it.
///
/// Every read of the provider reports this record, so a create and a later read cannot
/// disagree about its configuration or status.
#[derive(Debug, Clone)]
pub struct CapacityProviderRecord {
    pub capacity_provider_id: String,
    pub capacity_provider_arn: String,
    pub name: String,
    pub description: Option<String>,
    pub permissions_configuration: PermissionsConfiguration,
    pub compute_configuration: ComputeConfiguration,
    /// The tags the create carried. Held with the provider — it is what the request
    /// persisted — though no operation of this service reports a provider's tags yet
    /// (`ListTagsForResource` is still scaffolded).
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The status the provider reports until [`Self::settles_at`].
    pub transient_status: String,
    /// The status it reports from then on.
    pub settled_status: String,
    /// When the clock owes the move from one to the other. Stored, never slept on: a read
    /// materialises the transition the clock already made due.
    pub settles_at: SystemTime,
}

impl CapacityProviderRecord {
    /// The status at `now`: the transient one until the clock owes the transition, the
    /// settled one from then on. No read performs it — it is due at that instant whether or
    /// not anybody looks.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// This provider as `CreateCapacityProvider`'s response envelope: every member
    /// `CreateCapacityProviderResponse` declares, read off the record just written.
    pub fn create_output(&self, now: SystemTime) -> CreateCapacityProviderOutput {
        CreateCapacityProviderOutput {
            capacity_provider_arn: self.capacity_provider_arn.clone(),
            capacity_provider_id: self.capacity_provider_id.clone(),
            name: self.name.clone(),
            status: self.status_at(now).to_string(),
        }
    }

    /// This provider as `GetCapacityProvider`'s response envelope — the members
    /// [`Self::create_output`] renders, off the same record, plus the configuration the
    /// create persisted and the instants of the create and of the last write.
    ///
    /// `statusCode` and `statusReason` explain "a capacity provider that is not in the READY
    /// state" (`smithy_query shape=CapacityProviderStatusCode`), and every value the enum
    /// carries names a failure; a provider this emulator created is on its way to `READY`,
    /// so there is no reason code to report.
    pub fn get_output(&self, now: SystemTime) -> GetCapacityProviderOutput {
        GetCapacityProviderOutput {
            capacity_provider_arn: self.capacity_provider_arn.clone(),
            capacity_provider_id: self.capacity_provider_id.clone(),
            name: self.name.clone(),
            status: self.status_at(now).to_string(),
            description: self.description.clone(),
            status_code: None,
            status_reason: None,
            permissions_configuration: self.permissions_configuration.clone(),
            compute_configuration: self.compute_configuration.clone(),
            created_at: timestamp(self.created_at),
            last_updated_at: timestamp(self.updated_at),
        }
    }

    /// This provider as `ListCapacityProviders` reports it: the same record projected onto
    /// `CapacityProviderSummary`, so a listing and a read cannot disagree about its status.
    pub fn summary(&self, now: SystemTime) -> CapacityProviderSummary {
        CapacityProviderSummary {
            capacity_provider_arn: self.capacity_provider_arn.clone(),
            capacity_provider_id: self.capacity_provider_id.clone(),
            name: self.name.clone(),
            status: self.status_at(now).to_string(),
            last_updated_at: timestamp(self.updated_at),
        }
    }
}

impl ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// Mint a `capacityProviderId` for `name` that no stored capacity provider holds: the
    /// name + `-` + 10 alphanumerics, per `CapacityProviderId`'s pattern
    /// `^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$`.
    ///
    /// The stem is the name as sent: `CapacityProviderName` is
    /// `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`, exactly the stem that pattern admits, and the name's
    /// 1..48 `@length` with the 11 characters of the suffix lands inside the id's own 12..59.
    pub fn mint_capacity_provider_id(&mut self, name: &str, at: SystemTime) -> String {
        loop {
            self.mint_counter = self.mint_counter.wrapping_add(1);
            let candidate = format!("{name}-{}", suffix_for(name, self.mint_counter, at));
            if !self.capacity_providers.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    /// The capacity provider a previous request to `operation` carrying this `clientToken`
    /// produced, if the record is still held and the provider still exists.
    pub fn replayed_capacity_provider_result(
        &self,
        operation: &str,
        client_token: Option<&str>,
    ) -> Option<String> {
        let capacity_provider_id = self
            .capacity_provider_idempotency
            .get(&token_key(operation, client_token?))?;
        self.capacity_providers
            .contains_key(capacity_provider_id)
            .then(|| capacity_provider_id.clone())
    }

    /// Record what a `clientToken` produced for `operation`, evicting the oldest record once
    /// [`MAX_REMEMBERED_CLIENT_TOKENS`] are held, for the same reason
    /// [`Self::remember_payment_manager_client_token`] does.
    pub fn remember_capacity_provider_client_token(
        &mut self,
        operation: &str,
        client_token: Option<String>,
        capacity_provider_id: &str,
    ) {
        let Some(token) = client_token else {
            return;
        };
        remember_token(
            &mut self.capacity_provider_idempotency,
            &mut self.capacity_provider_idempotency_order,
            token_key(operation, &token),
            capacity_provider_id.to_string(),
        );
    }
}
