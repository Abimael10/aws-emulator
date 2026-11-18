// SPDX-License-Identifier: CC0-1.0

//! The training-session slice of this service's state: the [`TrainingSession`] record
//! `CreateTrainingSession` writes and `DescribeTrainingSession` replays, its ARN kind,
//! statuses, store bound, clock-owed span and the sentence a reader states an unknown
//! session with.
//!
//! A training session is the CONTROL-PLANE resource behind the SageMaker Training
//! Session Runtime data plane: this slice holds what the session IS — the LoRA
//! configuration it fine-tunes with, the base model it adapts, where its checkpoints are
//! written, the role it runs as and the status its compute has reached. The training and
//! sampling operations submitted against it are the runtime service's own records, so
//! nothing of them is kept here.

use super::*;

use crate::ops::types::{LoraConfig, TrainingSessionModelConfig, TrainingSessionOutputConfig};

/// The ARN resource kind a training session carries, i.e. the `<kind>` in
/// [`resource_arn`]. Fixed by `TrainingSessionArn`'s modelled pattern
/// (`^arn:(aws[a-z\-]*):sagemaker:[a-z0-9\-]+:[0-9]{12}:training-session/`), and named
/// once here so the create mints and the readers match the one kind.
pub const TRAINING_SESSION_ARN_KIND: &str = "training-session";

/// See [`MODEL_QUOTA`]. Bounds the training-session store the same way, and is what
/// `CreateTrainingSession` answers `ResourceLimitExceeded` past.
pub const TRAINING_SESSION_QUOTA: usize = 1_000;

/// How long after `CreateTrainingSession` the clock owes the `Created` -> `Running`
/// transition on a training session.
///
/// The operation "provisions compute resources for fine-tuning a foundation model"
/// (`CreateTrainingSession`'s doc), which is not instantaneous: the session is reported
/// `Created` until that compute is up and `Running` afterwards. AWS publishes no duration
/// for it, so the span is CHOSEN — the same three seconds this service gives a Studio
/// domain ([`DOMAIN_IN_SERVICE_AFTER`]) — and it is never zero, so a caller that
/// describes the session straight after creating it still observes `Created`. The due
/// instant is stored on the record and materialised by whichever reporting call first
/// runs at or after it; nothing sleeps.
pub const TRAINING_SESSION_RUNNING_AFTER: Duration = Duration::from_secs(3);

/// The `TrainingSessionStatus` members a created session passes through, from the model's
/// own enum (`smithy_query(query="shape",shape="TrainingSessionStatus")`). The enum also
/// carries the blocked, failed, scheduled, stop, expiry and deletion states, which the
/// create and the describe do not reach.
pub const TRAINING_SESSION_CREATED: &str = "Created";
/// See [`TRAINING_SESSION_CREATED`].
pub const TRAINING_SESSION_RUNNING: &str = "Running";

/// The kind label [`could_not_find`] names a training session by.
pub const TRAINING_SESSION_KIND_LABEL: &str = "training session";

/// How an operation that cannot resolve the training session a request names states it:
/// the [`could_not_find`] sentence over the NAME the caller sent.
///
/// A training session is addressed by `TrainingSessionName` and by nothing else — every
/// operation of the family takes that one member — so the identifier the refusal quotes
/// is the one the request carried, never an ARN synthesised from it. Stated once here so
/// the readers of this resource cannot word the same fact differently.
#[must_use]
pub fn training_session_not_found(name: &str) -> String {
    could_not_find(TRAINING_SESSION_KIND_LABEL, name)
}

/// A training session created by `CreateTrainingSession`, as `DescribeTrainingSession`
/// replays it.
///
/// `running_due_at` is the instant the clock owes the `Created` -> `Running` transition;
/// it is stored, never slept on, and materialised by whichever reporting call first runs
/// at or after it ([`Self::settle_status`]).
///
/// `model_package_group_arn` and `vpc_config` stay `None` when the create named neither:
/// the model documents no default for either, so the session reports no value rather than
/// one the request never asked for.
#[derive(Debug, Clone)]
pub struct TrainingSession {
    pub name: String,
    pub arn: String,
    pub status: String,
    pub model_package_group_arn: Option<String>,
    pub role_arn: String,
    pub lora_config: LoraConfig,
    pub model_config: TrainingSessionModelConfig,
    pub output_config: TrainingSessionOutputConfig,
    pub vpc_config: Option<VpcConfig>,
    pub tags: Vec<Tag>,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub running_due_at: SystemTime,
}

impl TrainingSession {
    /// Materialise the `Created` -> `Running` transition the clock already owes, the way
    /// [`MlflowTrackingServer::settle_status`] does for a tracking server: it was due at
    /// `running_due_at` whether or not anybody described the session, so the observer
    /// makes no difference to the result, and `LastModifiedTime` moves to the instant the
    /// transition was due rather than to "now".
    ///
    /// Answers whether the transition happened on this call, so the caller can record it.
    pub fn settle_status(&mut self, now: SystemTime) -> bool {
        if self.status == TRAINING_SESSION_CREATED && now >= self.running_due_at {
            self.status = TRAINING_SESSION_RUNNING.to_string();
            self.last_modified_time = self.running_due_at;
            return true;
        }
        false
    }
}

impl ComAmazonawsSagemakerSagemakerState {
    /// Whether another training session fits, i.e. the store is below
    /// [`TRAINING_SESSION_QUOTA`].
    pub fn training_sessions_below_quota(&self) -> bool {
        self.training_sessions.len() < TRAINING_SESSION_QUOTA
    }

    /// The training session `name` names, with the transition the clock already made due
    /// applied, or `None` when this account holds none under that name.
    ///
    /// For a REPORTING operation: a name no create has written answers `None` rather than
    /// being materialised here. Answers the session beside whether the transition happened
    /// on this call, so the caller can record the state change it materialised.
    pub fn advance_training_session(
        &mut self,
        name: &str,
        now: SystemTime,
    ) -> Option<(&TrainingSession, bool)> {
        let session = self.training_sessions.get_mut(name)?;
        let settled = session.settle_status(now);
        Some((session, settled))
    }

    /// Whether `arn` names a model package group of THIS account in `region` that the
    /// account does not hold — the one reading of a `ModelPackageGroupArn` reference this
    /// service resolves.
    ///
    /// A group ARN of another partition, Region or account names no record here, so it is
    /// not something this emulator can answer "does not exist" about: only an ARN this
    /// service itself mints ([`resource_arn`]) is resolved, and anything else is left
    /// alone.
    pub fn model_package_group_arn_is_unresolvable(&self, region: &str, arn: &str) -> bool {
        let Some(resource) = arn_resource(arn, region) else {
            return false;
        };
        let Some(name) = resource.strip_prefix(&format!("{MODEL_PACKAGE_GROUP_ARN_KIND}/")) else {
            return false;
        };
        !self.model_package_groups.contains_key(name)
    }
}
