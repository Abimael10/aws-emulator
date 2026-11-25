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
    DeleteCapacityProviderOutput, GetCapacityProviderOutput, PermissionsConfiguration,
    UpdateCapacityProviderOutput, UpdatedDescription,
};

/// The modelled not-found error for a capacity provider this account does not hold.
///
/// The `/capacity-providers` backend of this endpoint words a missing resource its own way —
/// "No capacity provider found with id " around the id the request named, as the recorded
/// 404 of `GetCapacityProvider` words it — rather than in the sentence the browser plane
/// uses ([`browser_not_found`]), which is why this is not [`super::resource_not_found`]
/// either. The sentence is fixed and the identifier is the caller's own value, taken
/// verbatim from the request. Shared by the handlers rather than written per operation, so
/// one provider cannot be reported missing in two different sentences by two operations.
pub fn capacity_provider_not_found(capacity_provider_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!(
            "No capacity provider found with id {capacity_provider_id}"
        )),
    }
}

// A capacity provider has NO duplicate-name refusal, which is why this module carries none of
// the control plane's conflict sentences: the provider's identity is the `capacityProviderId`
// this service mints ([`ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::mint_capacity_provider_id`]),
// the name being only the stem of it, so a create reusing a name is served and mints a second,
// independently identified provider.

/// The whole sentence a create whose OPERATOR ROLE this service cannot assume is refused
/// with, as the recorded `CreateCapacityProvider` words it.
///
/// Fixed text: it names no role, account or other value of the request — the two things to
/// check are the same whichever role was named — so every request refused for this reason
/// carries this one sentence, on the error itself and again on the `fieldList` entry.
pub const OPERATOR_ROLE_VALIDATION_FAILED_MESSAGE: &str =
    "Role validation failed for the operator role. Please verify that the role exists and \
     its trust policy allows assumption by this service";

/// The member a failure to assume the operator role is attributed to, as the dotted path the
/// framework names a request member by.
///
/// The role is the one "that AgentCore assumes to manage the capacity provider, including
/// launching, tagging, and terminating instances and their network interfaces"
/// (`smithy_query shape=PermissionsConfiguration member=capacityProviderOperatorRoleArn`), and
/// the subnets of the compute configuration's `vpcConfiguration` are where it has to launch
/// them — which is the member the recorded refusal attributes the failure to, and the one a
/// caller is pointed at. `ComputeConfiguration` is a union
/// (`smithy_query shape=ComputeConfiguration`), so the path runs through the variant the
/// request carried and a variant added to the model later has to name its own.
fn operator_role_field_path(compute_configuration: &ComputeConfiguration) -> &'static str {
    match compute_configuration {
        ComputeConfiguration::ec2Configuration(_) => {
            "computeConfiguration.ec2Configuration.vpcConfiguration.subnets"
        }
    }
}

/// The modelled validation error for an operator role this service could not assume.
///
/// `reason` says a field of the request failed validation, and the `fieldList` the shape
/// declares for exactly that carries the attributed member
/// ([`operator_role_field_path`]) — spelled as the JSON pointer into the request body this
/// `/capacity-providers` backend addresses a member with
/// (`crate::service::validation::pointer_form`, the tree's one spelling of one,
/// [`::service_common::json_pointer`]) rather than as the dotted path the walk reports.
pub fn operator_role_validation_failed(
    compute_configuration: &ComputeConfiguration,
) -> ValidationException {
    ValidationException {
        message: OPERATOR_ROLE_VALIDATION_FAILED_MESSAGE.to_string(),
        reason: VALIDATION_REASON_FIELD_VALIDATION_FAILED.to_string(),
        field_list: Some(vec![ValidationExceptionField {
            name: ::service_common::json_pointer::json_pointer(operator_role_field_path(
                compute_configuration,
            )),
            message: OPERATOR_ROLE_VALIDATION_FAILED_MESSAGE.to_string(),
        }]),
    }
}

/// Whether IAM reports that this service could not assume the capacity provider's OPERATOR
/// role — this account's IAM holds no role under the name the ARN carries, or the trust
/// policy of the role it does hold does not admit this service.
///
/// A capacity provider is MANAGED as that role — the service assumes it to launch, tag and
/// terminate the instances and their network interfaces — so the role is resolved before the
/// provider is minted, and a create naming one this service cannot assume is refused with
/// [`operator_role_validation_failed`] and nothing persisted.
///
/// The question, its grounds and its non-answers are the ONE reading every caller-supplied
/// role of this tree is judged by (`service_common::iam_role::role_is_unassumable`, which
/// [`super::execution_role_is_unassumable`] takes of an agent runtime's role): a refusal is
/// grounded in what IAM ANSWERED, so `false` — serve the create — for a value that names no
/// role of this account, an answer this emulator could not read, and an emulator with no IAM
/// mounted to ask.
pub async fn operator_role_is_unassumable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operator_role_arn: &str,
) -> bool {
    ::service_common::iam_role::role_is_unassumable(
        ctx,
        region,
        operator_role_arn,
        super::SERVICE_PRINCIPAL,
    )
    .await
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
pub const CAPACITY_PROVIDER_STATUS_UPDATING: &str = "UPDATING";
pub const CAPACITY_PROVIDER_STATUS_READY: &str = "READY";
/// "`DELETING` – The service is deleting the capacity provider"
/// (`smithy_query shape=CapacityProviderStatus`): what a provider whose deletion has been
/// ACCEPTED reports until the removal the clock owes it comes due.
pub const CAPACITY_PROVIDER_STATUS_DELETING: &str = "DELETING";

/// Whether a provider reporting `status` can take an accepted change right now.
///
/// A provider is modifiable once it is "available for use" — `READY` — and while it is
/// already `UPDATING`, where a further change is the one that wins; a provider whose own
/// asynchronous provisioning or removal is what it is doing (`CREATING`, `DELETING`) or has
/// failed at (`*_FAILED`) is not (`smithy_query shape=CapacityProviderStatus`).
pub fn capacity_provider_accepts_update(status: &str) -> bool {
    matches!(
        status,
        CAPACITY_PROVIDER_STATUS_READY | CAPACITY_PROVIDER_STATUS_UPDATING
    )
}

/// The modelled conflict for a change asked of a capacity provider that is not in a status
/// which admits one ([`capacity_provider_accepts_update`]): the request collides with what the
/// provider is already doing, so nothing is written.
///
/// The sentence is fixed and tells the caller what to wait for; its variable parts are the id
/// the request carried and the status the provider is in right now, spelled as the
/// `CapacityProviderStatus` token every response reports it in — worded as the sibling
/// resources of this control plane word the same refusal
/// ([`consent_portal_status_conflict`]).
pub fn capacity_provider_status_conflict(
    capacity_provider_id: &str,
    status: &str,
) -> ConflictException {
    ConflictException {
        message: Some(format!(
            "Capacity provider {capacity_provider_id} is in {status} status and cannot be \
             modified. Retry the request once its status is {CAPACITY_PROVIDER_STATUS_READY}."
        )),
    }
}

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

/// How long an ACCEPTED capacity provider deletion stays under way — the span the provider
/// reports `DELETING` for before the clock owes its removal.
///
/// `DeleteCapacityProvider` answers 202 carrying `status` (`CapacityProviderStatus`), so the
/// deletion is asynchronous: the call accepts it, it does not carry it out. AWS documents no
/// duration for it, so the span is chosen — the same one the sibling resources' transitions
/// take ([`super::AGENT_RUNTIME_REMOVAL_DELAY`]), long enough that a caller acting straight
/// after the delete still observes the provider going away. Stored as an instant on the
/// record and never slept on.
pub const CAPACITY_PROVIDER_REMOVAL_DELAY: Duration = super::AGENT_RUNTIME_REMOVAL_DELAY;

/// How this service words the refusal of a `DeleteCapacityProvider` for a provider agent
/// runtime versions are still attached to: what could not be done, the provider by the id the
/// request named it with, and what the caller has to delete before the provider can go, as
/// the recorded `DeleteCapacityProvider` words it.
///
/// "Before you delete a capacity provider, disassociate all agent runtimes and runtime
/// versions that reference it. If any references remain, the operation fails"
/// (`smithy_query doc shape=DeleteCapacityProvider`) — so a referenced provider is not
/// destroyed underneath the runtimes that launch instances from it: the delete is refused and
/// the caller deletes the attached versions first, rather than the references being silently
/// cascaded away. Nothing is wrong with the request's members — it is the stored state that
/// refuses it — so `reason` is [`VALIDATION_REASON_RESOURCE_CONFLICT`] and there is no
/// `fieldList`. The sentence around the id is fixed, the id quoted inside it; the id itself is
/// the one the request's path carried.
pub fn capacity_provider_in_use(capacity_provider_id: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "Cannot delete capacity provider '{capacity_provider_id}' because it still has \
             attached agent runtime versions. Delete all attached agent runtime versions \
             before deleting the capacity provider."
        ),
        reason: VALIDATION_REASON_RESOURCE_CONFLICT.to_string(),
        field_list: None,
    }
}

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
    /// When the clock owes this provider's REMOVAL, once a delete has been accepted for it;
    /// `None` for a provider nobody has deleted. Stored, never slept on, for the reason
    /// [`Self::settles_at`] is.
    pub removal_due_at: Option<SystemTime>,
}

impl CapacityProviderRecord {
    /// The status at `now`: the transient one until the clock owes the transition, the
    /// settled one from then on. No read performs it — it is due at that instant whether or
    /// not anybody looks.
    pub fn status_at(&self, now: SystemTime) -> &str {
        // A provider whose deletion has been accepted reports that it is going away,
        // whatever it was doing before: it is the one thing a client polling it needs to
        // see.
        if self.is_deleting() {
            return CAPACITY_PROVIDER_STATUS_DELETING;
        }
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// Whether this provider's deletion has been accepted and is still under way.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// Accept this provider's deletion: it reports `DELETING` from now on, and the clock owes
    /// its removal [`CAPACITY_PROVIDER_REMOVAL_DELAY`] later.
    ///
    /// A deletion already under way is NOT re-armed — `DeleteCapacityProvider` is
    /// `@idempotent` (`smithy_query operation=DeleteCapacityProvider`), so a repeat reports
    /// the same provider going away rather than pushing its removal further out.
    pub fn begin_deleting(&mut self, now: SystemTime) {
        if !self.is_deleting() {
            self.removal_due_at = Some(now + CAPACITY_PROVIDER_REMOVAL_DELAY);
        }
    }

    /// This provider as `DeleteCapacityProvider`'s response envelope: the id the request
    /// addressed it by and the status it reports now the deletion has been accepted.
    pub fn delete_output(&self, now: SystemTime) -> DeleteCapacityProviderOutput {
        DeleteCapacityProviderOutput {
            capacity_provider_id: self.capacity_provider_id.clone(),
            status: self.status_at(now).to_string(),
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

    /// Apply an accepted modification at `now`: the provider reports `UPDATING` until the
    /// clock owes it `READY` one settle span later, exactly as the create's own transition is
    /// owed, and `lastUpdatedAt` becomes the instant of this call.
    ///
    /// `description` is the request's `UpdatedDescription` wrapper, applied with the PATCH
    /// semantics the model states for it: "When present in an update request, the description
    /// is replaced with optionalValue. When absent, the description is left unchanged. To
    /// unset the description, include the wrapper with optionalValue not specified"
    /// (`smithy_query doc shape=UpdatedDescription`). Every other member of the provider —
    /// its name, its id, its ARN, its compute and permissions configuration — is NOT
    /// modifiable by an update ("Only the description can be changed. To change other
    /// configuration, such as instance types, networking, or storage, create a new capacity
    /// provider") and is left as the create stored it.
    ///
    /// A second change arriving while the first is still owed its transition is applied the
    /// same way, so the LAST write is the one the provider carries and the span it settles in
    /// runs from that write.
    pub fn accept_update(&mut self, description: Option<&UpdatedDescription>, now: SystemTime) {
        if let Some(wrapper) = description {
            self.description = wrapper.optional_value.clone();
        }
        self.transient_status = CAPACITY_PROVIDER_STATUS_UPDATING.to_string();
        self.settled_status = CAPACITY_PROVIDER_STATUS_READY.to_string();
        self.settles_at = now + CAPACITY_PROVIDER_SETTLE_DELAY;
        self.updated_at = now;
    }

    /// This provider as `UpdateCapacityProvider`'s response envelope: every member
    /// `UpdateCapacityProviderResponse` declares, read off the record the update just wrote,
    /// so the response and a later read cannot disagree about the status or the instants.
    pub fn update_output(&self, now: SystemTime) -> UpdateCapacityProviderOutput {
        UpdateCapacityProviderOutput {
            capacity_provider_arn: self.capacity_provider_arn.clone(),
            capacity_provider_id: self.capacity_provider_id.clone(),
            name: self.name.clone(),
            status: self.status_at(now).to_string(),
            created_at: timestamp(self.created_at),
            last_updated_at: timestamp(self.updated_at),
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
    ///
    /// The suffix is what tells two providers of ONE name apart, so this mint is asked for a
    /// name the account already holds as readily as for a fresh one, and the loop keeps
    /// drawing until the candidate id is free.
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

    /// Remove every capacity provider whose deletion the clock has already made due, with the
    /// idempotency records naming it.
    ///
    /// Called by every operation that reads the capacity-provider store, before it reads: the
    /// removal was owed at this instant whether or not anybody called, so a caller that
    /// deletes and then polls the provider sees `DELETING` and then the modelled 404 without
    /// anything sleeping for it.
    pub fn expire_deleted_capacity_providers(&mut self, now: SystemTime) {
        let due: Vec<String> = self
            .capacity_providers
            .iter()
            .filter(|(_, record)| record.removal_due_at.is_some_and(|at| now >= at))
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            self.capacity_providers.remove(&id);
            self.forget_capacity_provider_client_tokens_for(&id);
        }
    }

    /// Drop the idempotency records naming a capacity provider that no longer exists, for the
    /// same reason [`Self::forget_gateway_client_tokens_for`] does.
    fn forget_capacity_provider_client_tokens_for(&mut self, capacity_provider_id: &str) {
        self.capacity_provider_idempotency
            .retain(|_, id| id != capacity_provider_id);
        let live = &self.capacity_provider_idempotency;
        self.capacity_provider_idempotency_order
            .retain(|token| live.contains_key(token));
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
