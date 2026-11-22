// SPDX-License-Identifier: CC0-1.0

//! The custom evaluator (`/evaluators/`): one [`EvaluatorRecord`] per `evaluatorId`, holding
//! everything `CreateEvaluator` persisted, the `CREATING` -> `ACTIVE` and
//! `UPDATING` -> `ACTIVE` transitions the clock owes it, the removal an accepted deletion
//! owes it, and the refusals a missing, busy or in-use evaluator is reported with.
//!
//! Lives beside the stores in [`super`] rather than in any one `ops/` file, so the create,
//! the read, the update and the delete of an evaluator cannot drift from each other.
//!
//! An evaluator this account created is addressed by the `evaluatorId` the create minted —
//! `EvaluatorId` also admits the `Builtin.…` and `ThirdParty.…` forms of the evaluators the
//! SERVICE provides (`smithy_query shape=EvaluatorId`), which no create of this account
//! mints — so the store is keyed by that id and the name index only answers whether the
//! account already holds that name.

use super::*;

use crate::ops::types::{
    CreateEvaluatorInput, CreateEvaluatorOutput, DeleteEvaluatorOutput, EvaluatorConfig,
    EvaluatorReference, GetEvaluatorOutput, LlmAsAJudgeEvaluatorConfig, RatingScale,
    UpdateEvaluatorInput, UpdateEvaluatorOutput,
};

/// The ARN resource type an evaluator carries: `CustomEvaluatorArn` ends in
/// `…:evaluator/{evaluatorId}` (`smithy_query shape=CustomEvaluatorArn`).
const ARN_TYPE_EVALUATOR: &str = "evaluator";

/// `arn:aws:bedrock-agentcore:{region}:{account}:evaluator/{evaluatorId}` —
/// `CustomEvaluatorArn`'s form.
pub fn evaluator_arn(region: &str, evaluator_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("{ARN_TYPE_EVALUATOR}/{evaluator_id}"),
    )
}

/// `EvaluatorStatus` members this service drives. The enum's full value list is
/// `ACTIVE | CREATING | CREATE_FAILED | UPDATING | UPDATE_FAILED | DELETING`
/// (`smithy_query shape=EvaluatorStatus`); the two failed members belong to a provisioning
/// or a modification that failed, which nothing here makes happen.
pub const EVALUATOR_STATUS_CREATING: &str = "CREATING";
pub const EVALUATOR_STATUS_ACTIVE: &str = "ACTIVE";
pub const EVALUATOR_STATUS_UPDATING: &str = "UPDATING";
pub const EVALUATOR_STATUS_DELETING: &str = "DELETING";

/// `EvaluatorType` members a create of THIS account can produce: "`Custom` – A
/// customer-created evaluator", "`CustomCode` – A customer-created code-based evaluator",
/// "`CustomDerived` – A customer-created evaluator derived from an existing base evaluator"
/// (`smithy_query doc shape=GetEvaluatorResponse member=evaluatorType`). The `Builtin` and
/// `ThirdParty` members name the evaluators the service itself provides, which no create
/// mints.
pub const EVALUATOR_TYPE_CUSTOM: &str = "Custom";
pub const EVALUATOR_TYPE_CUSTOM_CODE: &str = "CustomCode";
pub const EVALUATOR_TYPE_CUSTOM_DERIVED: &str = "CustomDerived";

/// The `Provider` an evaluator this account created carries: "The source of the evaluator's
/// logic: Amazon Web Services, a third-party library, or you"
/// (`smithy_query doc shape=GetEvaluatorResponse member=provider`), and the logic of an
/// evaluator a caller created is the caller's own. The other members (`AWS`, `DeepEval`,
/// `AutoEval`) belong to the evaluators the service provides.
pub const EVALUATOR_PROVIDER_CUSTOM: &str = "Custom";

/// `IncludedData`'s member that withholds an evaluator's sensitive data:
/// "`METADATA_ONLY` returns evaluator metadata and model configuration without instructions
/// or rating scale" (`smithy_query doc shape=GetEvaluatorRequest member=includedData`); the
/// other member, `ALL_DATA`, is the default and returns the whole configuration.
pub const INCLUDED_DATA_METADATA_ONLY: &str = "METADATA_ONLY";

/// How long a freshly created or updated evaluator stays in its transient status before the
/// clock owes it `ACTIVE`, and how long one whose deletion was accepted stays readable in
/// `DELETING` before the clock owes its removal.
///
/// The create, the update and the delete are each accepted with HTTP 202 and report a
/// transient status (`smithy_query operation=CreateEvaluator,UpdateEvaluator,DeleteEvaluator`),
/// and AWS documents no duration for any of those transitions, so the span is chosen: the
/// same one the sibling resources' transitions take ([`super::REGISTRY_SETTLE_DELAY`]), long
/// enough that a caller acting straight after the call still observes the transient status.
pub const EVALUATOR_SETTLE_DELAY: Duration = super::REGISTRY_SETTLE_DELAY;

/// How many evaluators this emulator will hold.
///
/// `CreateEvaluator` models `ServiceQuotaExceededException` (HTTP 402) for exceeding the real
/// service's quota; this number is not a fidelity claim about that quota but the emulator's
/// own ceiling, set generously so no realistic workflow meets it, so a caller looping on
/// create cannot grow this process's memory without bound.
pub const MAX_EVALUATORS: usize = 1_000;

/// How this control plane spells the evaluator in a refusal that names it: the subject of the
/// duplicate-name sentence this plane shares, and of the sentences this family words itself,
/// so one plane does not name its resource two ways.
const EVALUATOR_LABEL: &str = "Evaluator";

/// The modelled conflict for an evaluator `name` this account already holds.
///
/// `CreateEvaluator`'s `evaluatorName` "Must be unique within your account"
/// (`smithy_query doc shape=CreateEvaluatorRequest member=evaluatorName`), and the refusal is
/// this control plane's fixed sentence ([`super::name_conflict`]) around the name the request
/// carried.
pub fn evaluator_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(EVALUATOR_LABEL, name)
}

/// The modelled not-found error for an evaluator this account does not hold: the one fixed
/// sentence this family words a missing evaluator with.
///
/// The identifier the request named is NOT echoed — it is the caller's own value, and the
/// remedy does not depend on it — so every operation that resolves an evaluator reports a
/// missing one in exactly these words.
pub fn evaluator_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("{EVALUATOR_LABEL} does not exist")),
    }
}

/// The modelled conflict for a change asked of an evaluator whose deletion has already been
/// accepted: the resource is on its way out, so the change collides with the removal the
/// clock owes it rather than being taken on a record that is about to go.
///
/// The sentence is fixed and tells the caller what the evaluator is doing instead; its
/// variable parts are the id the request carried and the status the evaluator is in right
/// now, as the enum spells it — the same token every response reports it in.
pub fn evaluator_status_conflict(evaluator_id: &str, status: &str) -> ConflictException {
    ConflictException {
        message: Some(format!(
            "{EVALUATOR_LABEL} {evaluator_id} is in {status} status and cannot be modified."
        )),
    }
}

/// The modelled conflict for a change asked of an evaluator an online evaluation
/// configuration is evaluating with: the evaluator is "locked for modification due to being
/// referenced by active online evaluation configurations"
/// (`smithy_query doc shape=GetEvaluatorResponse member=lockedForModification`), which is the
/// same condition a read of it reports, so what the read says and what a write is refused
/// with cannot disagree.
pub fn evaluator_locked_conflict(evaluator_id: &str) -> ConflictException {
    ConflictException {
        message: Some(format!(
            "{EVALUATOR_LABEL} {evaluator_id} is referenced by an active online evaluation \
             configuration and cannot be modified."
        )),
    }
}

/// One evaluator: everything `CreateEvaluator` persisted for it and the transitions the clock
/// owes it.
///
/// Every read of the evaluator reports this record, so a create, an update and a later read
/// cannot disagree about its configuration or its status.
#[derive(Debug, Clone)]
pub struct EvaluatorRecord {
    pub evaluator_id: String,
    pub evaluator_arn: String,
    pub name: String,
    pub description: Option<String>,
    pub evaluator_config: EvaluatorConfig,
    pub level: String,
    pub kms_key_arn: Option<String>,
    /// The tags the create carried. Held with the evaluator — it is what the request
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
    /// The instant an accepted deletion removes this record, once `DeleteEvaluator` has
    /// accepted one; `None` for an evaluator nobody has deleted. Stored, never slept on: the
    /// removal is the clock's, materialised by
    /// [`expire_deleted_evaluators`](ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::expire_deleted_evaluators)
    /// on the next call that reads this store.
    pub removal_due_at: Option<SystemTime>,
}

impl EvaluatorRecord {
    /// The evaluator a create stores for the request `requested` in `region`, under the id
    /// that create minted: `CREATING`, with the transition to `ACTIVE` owed by the clock
    /// rather than by the call.
    pub fn created(
        region: &str,
        evaluator_id: String,
        requested: CreateEvaluatorInput,
        now: SystemTime,
    ) -> Self {
        Self {
            evaluator_arn: evaluator_arn(region, &evaluator_id),
            evaluator_id,
            name: requested.evaluator_name,
            description: requested.description,
            evaluator_config: requested.evaluator_config,
            level: requested.level,
            kms_key_arn: requested.kms_key_arn,
            tags: sorted_map(requested.tags),
            created_at: now,
            updated_at: now,
            transient_status: EVALUATOR_STATUS_CREATING.to_string(),
            settled_status: EVALUATOR_STATUS_ACTIVE.to_string(),
            settles_at: now + EVALUATOR_SETTLE_DELAY,
            removal_due_at: None,
        }
    }

    /// The status this evaluator reports at `now`: the transient one until the transition the
    /// clock owes comes due, the settled one afterwards. No read performs it — it is due at
    /// that instant whether or not anybody looks.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// Whether a deletion has been accepted for this evaluator: it reports `DELETING` and the
    /// clock owes its removal.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// The `EvaluatorType` this evaluator reports, read off the `EvaluatorConfig` member it
    /// was configured with: an LLM-as-a-Judge evaluator is a customer-created evaluator, one
    /// configured with a Lambda function is a code-based one, and one configured from a base
    /// evaluator is a derived one.
    ///
    /// Derived from the stored configuration rather than recorded beside it, so an update
    /// that replaces the configuration cannot leave the reported type behind.
    pub fn evaluator_type(&self) -> &'static str {
        match &self.evaluator_config {
            EvaluatorConfig::llmAsAJudge(_) => EVALUATOR_TYPE_CUSTOM,
            EvaluatorConfig::codeBased(_) => EVALUATOR_TYPE_CUSTOM_CODE,
            EvaluatorConfig::Derived(_) => EVALUATOR_TYPE_CUSTOM_DERIVED,
        }
    }

    /// Apply an accepted modification at `now`, with the PATCH semantics
    /// `UpdateEvaluatorRequest` states by declaring each editable member optional: a member
    /// the request carries replaces the stored one, a member it leaves out leaves the stored
    /// one alone. The evaluator's name, its tags, its id and its ARN are not modifiable by
    /// this operation — the request carries no member for them — so they stay as the create
    /// stored them.
    ///
    /// The evaluator reports `UPDATING` until the clock owes it `ACTIVE` one settle span
    /// later, as the create's own transition is owed.
    pub fn accept_update(&mut self, requested: UpdateEvaluatorInput, now: SystemTime) {
        if let Some(description) = requested.description {
            self.description = Some(description);
        }
        if let Some(evaluator_config) = requested.evaluator_config {
            self.evaluator_config = evaluator_config;
        }
        if let Some(level) = requested.level {
            self.level = level;
        }
        if let Some(kms_key_arn) = requested.kms_key_arn {
            self.kms_key_arn = Some(kms_key_arn);
        }
        self.transient_status = EVALUATOR_STATUS_UPDATING.to_string();
        self.settled_status = EVALUATOR_STATUS_ACTIVE.to_string();
        self.settles_at = now + EVALUATOR_SETTLE_DELAY;
        self.updated_at = now;
    }

    /// Accept a deletion at `now`: the evaluator reports `DELETING` from this instant on and
    /// the clock owes its removal one settle span later.
    ///
    /// Accepted whatever the evaluator was doing — a create still settling is removed too,
    /// the resource is on its way out either way — and idempotent, as `DeleteEvaluator` is
    /// (`@idempotent`): a repeat while the deletion is still under way leaves the record and
    /// the instant it is owed alone rather than re-arming the clock.
    pub fn accept_deletion(&mut self, now: SystemTime) {
        if self.is_deleting() {
            return;
        }
        self.transient_status = EVALUATOR_STATUS_DELETING.to_string();
        self.settled_status = EVALUATOR_STATUS_DELETING.to_string();
        self.settles_at = now;
        self.removal_due_at = Some(now + EVALUATOR_SETTLE_DELAY);
        self.updated_at = now;
    }

    /// This evaluator as `CreateEvaluator`'s response envelope: every member
    /// `CreateEvaluatorResponse` declares, read off the record just written.
    pub fn create_output(&self, now: SystemTime) -> CreateEvaluatorOutput {
        CreateEvaluatorOutput {
            evaluator_arn: self.evaluator_arn.clone(),
            evaluator_id: self.evaluator_id.clone(),
            created_at: timestamp(self.created_at),
            status: self.status_at(now).to_string(),
        }
    }

    /// This evaluator as `GetEvaluator`'s response envelope — the whole configuration the
    /// create and every accepted update left on the record, the type that configuration makes
    /// it, the provider a customer-created evaluator carries, and whether an online
    /// evaluation configuration has it locked for modification.
    ///
    /// `included_data` is what the request asked to be reported: `METADATA_ONLY` withholds
    /// the evaluator's instructions and rating scale, which are the sensitive members of an
    /// LLM-as-a-Judge configuration; every other configuration carries neither, so it is
    /// reported whole either way.
    pub fn get_output(
        &self,
        now: SystemTime,
        included_data: Option<&str>,
        locked_for_modification: bool,
    ) -> GetEvaluatorOutput {
        GetEvaluatorOutput {
            evaluator_arn: self.evaluator_arn.clone(),
            evaluator_id: self.evaluator_id.clone(),
            evaluator_name: self.name.clone(),
            description: self.description.clone(),
            evaluator_config: self.reported_config(included_data),
            evaluator_type: Some(self.evaluator_type().to_string()),
            provider: Some(EVALUATOR_PROVIDER_CUSTOM.to_string()),
            level: self.level.clone(),
            status: self.status_at(now).to_string(),
            created_at: timestamp(self.created_at),
            updated_at: timestamp(self.updated_at),
            locked_for_modification: Some(locked_for_modification),
            kms_key_arn: self.kms_key_arn.clone(),
        }
    }

    /// This evaluator as `UpdateEvaluator`'s response envelope — read off the record the
    /// accepted change left, with the status it now reports and the instant of that change.
    pub fn update_output(&self, now: SystemTime) -> UpdateEvaluatorOutput {
        UpdateEvaluatorOutput {
            evaluator_arn: self.evaluator_arn.clone(),
            evaluator_id: self.evaluator_id.clone(),
            updated_at: timestamp(self.updated_at),
            status: self.status_at(now).to_string(),
        }
    }

    /// This evaluator as `DeleteEvaluator`'s response envelope: the evaluator the deletion
    /// was accepted for and the status it reports from that acceptance on.
    pub fn delete_output(&self, now: SystemTime) -> DeleteEvaluatorOutput {
        DeleteEvaluatorOutput {
            evaluator_arn: self.evaluator_arn.clone(),
            evaluator_id: self.evaluator_id.clone(),
            status: self.status_at(now).to_string(),
        }
    }

    /// The configuration a read reports for `included_data`.
    ///
    /// `ALL_DATA` — the default — "returns the full evaluator including decrypted instructions
    /// and rating scale", while `METADATA_ONLY` "returns evaluator metadata and model
    /// configuration without instructions or rating scale"
    /// (`smithy_query doc shape=GetEvaluatorRequest member=includedData`).
    /// `LlmAsAJudgeEvaluatorConfig` declares both of those members required, so a withheld
    /// one is reported empty rather than left out: the model configuration beside them is
    /// reported in full, and the stored record keeps what the create persisted.
    fn reported_config(&self, included_data: Option<&str>) -> EvaluatorConfig {
        match (&self.evaluator_config, included_data) {
            (EvaluatorConfig::llmAsAJudge(config), Some(INCLUDED_DATA_METADATA_ONLY)) => {
                EvaluatorConfig::llmAsAJudge(LlmAsAJudgeEvaluatorConfig {
                    instructions: String::new(),
                    rating_scale: withheld_rating_scale(&config.rating_scale),
                    model_config: config.model_config.clone(),
                })
            }
            _ => self.evaluator_config.clone(),
        }
    }
}

/// A rating scale with its definitions withheld, keeping the `RatingScale` member the stored
/// scale is carried in: the kind of scale is metadata, the definitions on it are the data
/// `METADATA_ONLY` asks not to be returned.
fn withheld_rating_scale(scale: &RatingScale) -> RatingScale {
    match scale {
        RatingScale::Numerical(_) => RatingScale::Numerical(Vec::new()),
        RatingScale::Categorical(_) => RatingScale::Categorical(Vec::new()),
    }
}

impl ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// Mint an `evaluatorId` for `name` that no stored evaluator holds: the name + `-` + 10
    /// alphanumerics, the only form of `EvaluatorId` a create of this account mints
    /// (`^[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`).
    ///
    /// The stem is the name as sent: `CustomEvaluatorName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`,
    /// a subset of the stem that pattern admits, and its 48 characters with the 11 of the
    /// suffix land inside the id's own 111.
    pub fn mint_evaluator_id(&mut self, name: &str, at: SystemTime) -> String {
        loop {
            self.mint_counter = self.mint_counter.wrapping_add(1);
            let candidate = format!("{name}-{}", suffix_for(name, self.mint_counter, at));
            if !self.evaluators.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    /// The evaluator a previous request to `operation` carrying this `clientToken` produced,
    /// if the record is still held and the evaluator still exists.
    pub fn replayed_evaluator_result(
        &self,
        operation: &str,
        client_token: Option<&str>,
    ) -> Option<String> {
        let evaluator_id = self
            .evaluator_idempotency
            .get(&token_key(operation, client_token?))?;
        self.evaluators
            .contains_key(evaluator_id)
            .then(|| evaluator_id.clone())
    }

    /// Record what a `clientToken` produced for `operation`, evicting the oldest record once
    /// [`MAX_REMEMBERED_CLIENT_TOKENS`] are held, for the same reason every other
    /// `remember_*_client_token` of this store does.
    pub fn remember_evaluator_client_token(
        &mut self,
        operation: &str,
        client_token: Option<String>,
        evaluator_id: &str,
    ) {
        let Some(token) = client_token else {
            return;
        };
        remember_token(
            &mut self.evaluator_idempotency,
            &mut self.evaluator_idempotency_order,
            token_key(operation, &token),
            evaluator_id.to_string(),
        );
    }

    /// Remove every evaluator whose deletion the clock has already made due, with the name it
    /// reserved.
    ///
    /// `DeleteEvaluator` ACCEPTS the deletion (202) and the evaluator reports `DELETING`
    /// until then; the record itself goes when the span it owes passes, so a caller polling it
    /// sees `DELETING` and then the modelled 404, and the name it held is free again. The
    /// removal is owed by the CLOCK — it was due at this instant whether or not anybody called
    /// — so every operation that reads this store materialises it first, and nothing ever
    /// sleeps for it.
    pub fn expire_deleted_evaluators(&mut self, now: SystemTime) {
        let expired: Vec<String> = self
            .evaluators
            .iter()
            .filter(|(_, record)| record.removal_due_at.is_some_and(|at| now >= at))
            .map(|(id, _)| id.clone())
            .collect();
        for evaluator_id in expired {
            if let Some(record) = self.evaluators.remove(&evaluator_id) {
                self.evaluator_names.remove(&record.name);
            }
        }
    }

    /// Whether `evaluator_id` is locked for modification: whether any online evaluation
    /// configuration this account holds names it among the evaluators it evaluates with AND
    /// has its evaluation running ([`ONLINE_EVALUATION_EXECUTION_ENABLED`]) — the evaluator is
    /// in use by that evaluation, which is what the lock is about.
    ///
    /// Answered from the configuration store rather than recorded on the evaluator, so a
    /// configuration created, changed or removed after the evaluator cannot leave a stale lock
    /// behind.
    pub fn evaluator_locked_for_modification(&self, evaluator_id: &str) -> bool {
        self.online_evaluation_configs.values().any(|config| {
            config.execution_status == ONLINE_EVALUATION_EXECUTION_ENABLED
                && config.evaluators.iter().any(|reference| match reference {
                    EvaluatorReference::evaluatorId(referenced) => referenced == evaluator_id,
                })
        })
    }
}
