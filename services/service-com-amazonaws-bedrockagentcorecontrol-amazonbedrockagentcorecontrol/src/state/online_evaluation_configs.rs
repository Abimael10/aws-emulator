// SPDX-License-Identifier: CC0-1.0

//! The online evaluation configuration (`/online-evaluation-configs/`): one
//! [`OnlineEvaluationConfigRecord`] per `onlineEvaluationConfigId`, holding everything
//! `CreateOnlineEvaluationConfig` persisted, whether the evaluation it describes is running,
//! and the `CREATING` -> `ACTIVE` transition the clock owes it, so the create and every
//! later read answer from one store.
//!
//! Lives beside the stores in [`super`] rather than in any one `ops/` file, so the create
//! and every read of a configuration cannot drift from each other.
//!
//! A configuration is addressed only by the `onlineEvaluationConfigId` the create minted
//! (`OnlineEvaluationConfigId` admits nothing else,
//! `smithy_query shape=OnlineEvaluationConfigId`), so the store is keyed by it and the name
//! index only answers whether the account already holds that name.

use super::*;

use crate::ops::types::{
    ClusteringConfig, CreateOnlineEvaluationConfigInput, CreateOnlineEvaluationConfigOutput,
    DataSourceConfig, EvaluatorReference, GetOnlineEvaluationConfigOutput, Insight, OutputConfig,
    Rule,
};

/// The ARN resource type an online evaluation configuration carries:
/// `OnlineEvaluationConfigArn` ends in `…:online-evaluation-config/{onlineEvaluationConfigId}`
/// (`smithy_query shape=OnlineEvaluationConfigArn`).
const ARN_TYPE_ONLINE_EVALUATION_CONFIG: &str = "online-evaluation-config";

/// `arn:aws:bedrock-agentcore:{region}:{account}:online-evaluation-config/{id}` —
/// `OnlineEvaluationConfigArn`'s form.
pub fn online_evaluation_config_arn(region: &str, online_evaluation_config_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("{ARN_TYPE_ONLINE_EVALUATION_CONFIG}/{online_evaluation_config_id}"),
    )
}

/// `OnlineEvaluationConfigStatus` members this service drives. The enum's full value list is
/// `ACTIVE | CREATING | CREATE_FAILED | UPDATING | UPDATE_FAILED | DELETING | ERROR`
/// (`smithy_query shape=OnlineEvaluationConfigStatus`); the failed and `ERROR` members belong
/// to a provisioning or an evaluation that failed, which nothing here makes happen.
pub const ONLINE_EVALUATION_CONFIG_STATUS_CREATING: &str = "CREATING";
pub const ONLINE_EVALUATION_CONFIG_STATUS_ACTIVE: &str = "ACTIVE";

/// `OnlineEvaluationExecutionStatus`'s two values — whether the evaluation this
/// configuration describes is running (`smithy_query shape=OnlineEvaluationExecutionStatus`).
pub const ONLINE_EVALUATION_EXECUTION_ENABLED: &str = "ENABLED";
pub const ONLINE_EVALUATION_EXECUTION_DISABLED: &str = "DISABLED";

/// How long a freshly created online evaluation configuration stays `CREATING` before the
/// clock owes it `ACTIVE`.
///
/// The create is accepted with HTTP 202 and reports a transient status
/// (`smithy_query operation=CreateOnlineEvaluationConfig`), and AWS documents no duration for
/// the transition, so the span is chosen: the same one the sibling resources' transitions
/// take ([`super::REGISTRY_SETTLE_DELAY`]), long enough that a caller acting straight after
/// the create still observes the transient status.
pub const ONLINE_EVALUATION_CONFIG_SETTLE_DELAY: Duration = super::REGISTRY_SETTLE_DELAY;

/// How many online evaluation configurations this emulator will hold.
///
/// `CreateOnlineEvaluationConfig` models `ServiceQuotaExceededException` (HTTP 402) for
/// exceeding the real service's quota; this number is not a fidelity claim about that quota
/// but the emulator's own ceiling, set generously so no realistic workflow meets it, so a
/// caller looping on create cannot grow this process's memory without bound.
pub const MAX_ONLINE_EVALUATION_CONFIGS: usize = 1_000;

/// How this control plane spells the online evaluation configuration in a refusal that names
/// it — the resource as the recorded refusals of this family spell it, so one plane does not
/// name its resource two ways.
const ONLINE_EVALUATION_CONFIG_LABEL: &str = "Online evaluation configuration";

/// The modelled conflict for an online evaluation configuration name this account already
/// holds.
///
/// `CreateOnlineEvaluationConfig`'s name "must be unique within your account"
/// (`smithy_query doc shape=CreateOnlineEvaluationConfigRequest
/// member=onlineEvaluationConfigName`), and this family words the collision in its own fixed
/// sentence rather than through this control plane's shared [`super::name_conflict`]: the
/// recorded refusal names the resource and what is wrong with the request and echoes neither
/// the name that collided nor the configuration already holding it, so the sentence is the
/// same for every create that asks for a name this account holds.
pub fn online_evaluation_config_name_conflict() -> ConflictException {
    ConflictException {
        message: Some(format!(
            "{ONLINE_EVALUATION_CONFIG_LABEL} with same name already exists"
        )),
    }
}

/// The modelled not-found error for an online evaluation configuration this account does not
/// hold: the identifier the request carried, in the sentence this plane words a missing
/// configuration with.
///
/// The text around the identifier is fixed and the identifier is the caller's own value, so
/// every operation that resolves a configuration reports a missing one identically.
pub fn online_evaluation_config_not_found(
    online_evaluation_config_id: &str,
) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!(
            "{ONLINE_EVALUATION_CONFIG_LABEL} with ID {online_evaluation_config_id} does not \
             exist"
        )),
    }
}

/// One online evaluation configuration: everything `CreateOnlineEvaluationConfig` persisted
/// for it, whether its evaluation is running, and the transition the clock owes it.
///
/// Every read of the configuration reports this record, so a create and a later read cannot
/// disagree about its configuration, its status or its execution status.
#[derive(Debug, Clone)]
pub struct OnlineEvaluationConfigRecord {
    pub online_evaluation_config_id: String,
    pub online_evaluation_config_arn: String,
    pub name: String,
    pub description: Option<String>,
    pub rule: Rule,
    pub data_source_config: DataSourceConfig,
    /// The evaluators applied during the evaluation. `evaluators` is optional on the create
    /// and carries the modelled default `[]` on a read
    /// (`smithy_query operation=GetOnlineEvaluationConfig`), so a create that named none
    /// stores the empty list the read reports.
    pub evaluators: Vec<EvaluatorReference>,
    pub insights: Option<Vec<Insight>>,
    pub clustering_config: Option<ClusteringConfig>,
    pub output_config: Option<OutputConfig>,
    pub evaluation_execution_role_arn: String,
    /// Whether the evaluation this configuration describes is running:
    /// [`ONLINE_EVALUATION_EXECUTION_ENABLED`] for a create that asked for it on creation,
    /// [`ONLINE_EVALUATION_EXECUTION_DISABLED`] otherwise.
    pub execution_status: String,
    /// The tags the create carried. Held with the configuration — it is what the request
    /// persisted — though no operation of this service reports its tags yet
    /// (`ListTagsForResource` is still scaffolded).
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The status until [`settles_at`](Self::settles_at) passes.
    pub transient_status: String,
    /// The status from [`settles_at`](Self::settles_at) onwards.
    pub settled_status: String,
    /// The instant the transient status is owed its transition. Stored, never slept on: a
    /// read materialises the transition the clock already made due.
    pub settles_at: SystemTime,
}

impl OnlineEvaluationConfigRecord {
    /// The configuration a create stores for the request `requested` in `region`, under the
    /// id that create minted: `CREATING`, with the transition to `ACTIVE` owed by the clock
    /// rather than by the call.
    ///
    /// `enableOnCreate` is "Whether to enable the online evaluation configuration immediately
    /// upon creation. If true, evaluation begins automatically"
    /// (`smithy_query doc shape=CreateOnlineEvaluationConfigRequest member=enableOnCreate`),
    /// so it is what the stored execution status is read off.
    pub fn created(
        region: &str,
        online_evaluation_config_id: String,
        requested: CreateOnlineEvaluationConfigInput,
        now: SystemTime,
    ) -> Self {
        Self {
            online_evaluation_config_arn: online_evaluation_config_arn(
                region,
                &online_evaluation_config_id,
            ),
            online_evaluation_config_id,
            name: requested.online_evaluation_config_name,
            description: requested.description,
            rule: requested.rule,
            data_source_config: requested.data_source_config,
            evaluators: requested.evaluators.unwrap_or_default(),
            insights: requested.insights,
            clustering_config: requested.clustering_config,
            output_config: requested.output_config,
            evaluation_execution_role_arn: requested.evaluation_execution_role_arn,
            execution_status: if requested.enable_on_create {
                ONLINE_EVALUATION_EXECUTION_ENABLED.to_string()
            } else {
                ONLINE_EVALUATION_EXECUTION_DISABLED.to_string()
            },
            tags: sorted_map(requested.tags),
            created_at: now,
            updated_at: now,
            transient_status: ONLINE_EVALUATION_CONFIG_STATUS_CREATING.to_string(),
            settled_status: ONLINE_EVALUATION_CONFIG_STATUS_ACTIVE.to_string(),
            settles_at: now + ONLINE_EVALUATION_CONFIG_SETTLE_DELAY,
        }
    }

    /// The status this configuration reports at `now`: the transient one until the transition
    /// the clock owes comes due, the settled one afterwards. No read performs it — it is due
    /// at that instant whether or not anybody looks.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// This configuration as `CreateOnlineEvaluationConfig`'s response envelope: every member
    /// `CreateOnlineEvaluationConfigResponse` declares, read off the record just written.
    ///
    /// `failureReason` is "The reason for failure if the online evaluation configuration
    /// creation or execution failed"
    /// (`smithy_query doc shape=CreateOnlineEvaluationConfigResponse member=failureReason`),
    /// and a configuration this emulator created is on its way to `ACTIVE` with nothing
    /// failed, so there is no reason to report.
    pub fn create_output(&self, now: SystemTime) -> CreateOnlineEvaluationConfigOutput {
        CreateOnlineEvaluationConfigOutput {
            online_evaluation_config_arn: self.online_evaluation_config_arn.clone(),
            online_evaluation_config_id: self.online_evaluation_config_id.clone(),
            created_at: timestamp(self.created_at),
            output_config: self.output_config.clone(),
            status: self.status_at(now).to_string(),
            execution_status: self.execution_status.clone(),
            failure_reason: None,
        }
    }

    /// This configuration as `GetOnlineEvaluationConfig`'s response envelope — the members
    /// the create reports, off the same record, plus the whole configuration the create
    /// persisted and the instant of the last write.
    pub fn get_output(&self, now: SystemTime) -> GetOnlineEvaluationConfigOutput {
        GetOnlineEvaluationConfigOutput {
            online_evaluation_config_arn: self.online_evaluation_config_arn.clone(),
            online_evaluation_config_id: self.online_evaluation_config_id.clone(),
            online_evaluation_config_name: self.name.clone(),
            description: self.description.clone(),
            rule: self.rule.clone(),
            data_source_config: self.data_source_config.clone(),
            evaluators: Some(self.evaluators.clone()),
            insights: self.insights.clone(),
            clustering_config: self.clustering_config.clone(),
            output_config: self.output_config.clone(),
            evaluation_execution_role_arn: Some(self.evaluation_execution_role_arn.clone()),
            status: self.status_at(now).to_string(),
            execution_status: self.execution_status.clone(),
            created_at: timestamp(self.created_at),
            updated_at: timestamp(self.updated_at),
            failure_reason: None,
        }
    }
}

impl ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// Mint an `onlineEvaluationConfigId` for `name` that no stored configuration holds: the
    /// name + `-` + 10 alphanumerics, per `OnlineEvaluationConfigId`'s pattern
    /// `^[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`.
    ///
    /// The stem is the name as sent: `EvaluationConfigName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`,
    /// a subset of the stem that pattern admits, and its 48 characters with the 11 of the
    /// suffix land inside the id's own 101.
    pub fn mint_online_evaluation_config_id(&mut self, name: &str, at: SystemTime) -> String {
        loop {
            self.mint_counter = self.mint_counter.wrapping_add(1);
            let candidate = format!("{name}-{}", suffix_for(name, self.mint_counter, at));
            if !self.online_evaluation_configs.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    /// The online evaluation configuration a previous request to `operation` carrying this
    /// `clientToken` produced, if the record is still held and the configuration still
    /// exists.
    pub fn replayed_online_evaluation_config_result(
        &self,
        operation: &str,
        client_token: Option<&str>,
    ) -> Option<String> {
        let online_evaluation_config_id = self
            .online_evaluation_config_idempotency
            .get(&token_key(operation, client_token?))?;
        self.online_evaluation_configs
            .contains_key(online_evaluation_config_id)
            .then(|| online_evaluation_config_id.clone())
    }

    /// Record what a `clientToken` produced for `operation`, evicting the oldest record once
    /// [`MAX_REMEMBERED_CLIENT_TOKENS`] are held, for the same reason
    /// [`Self::remember_capacity_provider_client_token`] does.
    pub fn remember_online_evaluation_config_client_token(
        &mut self,
        operation: &str,
        client_token: Option<String>,
        online_evaluation_config_id: &str,
    ) {
        let Some(token) = client_token else {
            return;
        };
        remember_token(
            &mut self.online_evaluation_config_idempotency,
            &mut self.online_evaluation_config_idempotency_order,
            token_key(operation, &token),
            online_evaluation_config_id.to_string(),
        );
    }
}
