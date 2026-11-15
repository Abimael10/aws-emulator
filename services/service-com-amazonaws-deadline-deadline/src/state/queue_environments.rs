// SPDX-License-Identifier: CC0-1.0

//! The queue environments this store holds: what `CreateQueueEnvironment` writes, and what
//! `GetQueueEnvironment` reports.
//!
//! An environment is reached only through its own queue, exactly as the model's paths nest
//! it (`/2023-10-12/farms/{farmId}/queues/{queueId}/environments/{queueEnvironmentId}`), so
//! it hangs off the queue the way a job does and an environment of another queue is not
//! found through this one.

use std::time::SystemTime;

use super::ComAmazonawsDeadlineDeadlineState;

/// `resourceType` this service reports for a queue environment in
/// `ResourceNotFoundException`.
///
/// Spelled the way the API path spells that resource, like every other type this service
/// refuses — the segment `/environments/` names it `environment`.
pub const RESOURCE_TYPE_ENVIRONMENT: &str = "environment";

/// How many environments one queue holds at once. See [`super::MAX_FARMS`] for why each
/// level of this store carries a bound of its own.
pub const MAX_ENVIRONMENTS_PER_QUEUE: usize = 500;

/// A persisted queue environment, as `CreateQueueEnvironment` stored it under its queue.
#[derive(Debug, Clone)]
pub struct QueueEnvironment {
    pub queue_environment_id: String,
    pub queue_id: String,
    pub farm_id: String,
    /// The name the stored template declares for the environment it describes, resolved
    /// once at create time (see [`super::environment_template_name`]), so every later read
    /// reports the same name without parsing the document again.
    pub name: String,
    pub priority: i32,
    pub template_type: String,
    pub template: String,
    pub created_at: SystemTime,
    pub created_by: String,
    pub updated_at: Option<SystemTime>,
    pub updated_by: Option<String>,
}

impl ComAmazonawsDeadlineDeadlineState {
    /// The environment `queue_environment_id` names UNDER `farm_id`/`queue_id`: an
    /// environment of another queue is not found here, because the model reaches an
    /// environment only through its own queue's path.
    pub fn queue_environment(
        &self,
        farm_id: &str,
        queue_id: &str,
        queue_environment_id: &str,
    ) -> Option<&QueueEnvironment> {
        self.queue(farm_id, queue_id)?
            .environments
            .get(queue_environment_id)
    }
}

/// The 404 this service answers for a queue environment it does not hold, named and worded
/// the way [`super::not_found`] names and words every other missing resource.
pub fn queue_environment_not_found(
    queue_environment_id: &str,
) -> crate::ops::types::ResourceNotFoundException {
    super::not_found(RESOURCE_TYPE_ENVIRONMENT, queue_environment_id)
}

/// The 400 this service answers for an environment template that declares no name.
///
/// A queue environment's `name` is `@required` on every read of it and no request member
/// carries it, so a template with nothing to read there describes no environment this
/// service could report. One place to change: the sentence this error class carries is not
/// recoverable from the replay harness, which reports only what the emulator answered, so
/// it is this crate's own wording for the class until a recording names it. The member at
/// fault is the one the caller has to fix.
pub fn environment_template_unnamed() -> crate::ops::types::ValidationException {
    super::field_validation_failed(
        "template",
        "The environment template doesn't name its environment: declare a name on the \
         template's environment.",
    )
}
