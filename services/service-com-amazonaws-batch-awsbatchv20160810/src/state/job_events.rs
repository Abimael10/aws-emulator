// SPDX-License-Identifier: CC0-1.0

//! What a `Batch Job State Change` event CARRIES: the record one transition queues, and
//! the `detail` it is rendered as.
//!
//! The rendering is done where the change happens, off the stored job, so an event reports
//! the job as it was at that transition rather than as it is when the queue is drained
//! ([`super::publish_job_state_changes`] is what publishes them).

use super::*;

/// One job state change waiting to be published to EventBridge.
///
/// The `detail` is rendered WHEN THE CHANGE HAPPENS (see
/// [`job_state_change_detail`]), so the event reports the job as it was at that
/// transition rather than as it is when the queue is drained.
#[derive(Debug, Clone)]
pub struct JobStateChange {
    /// The job's ARN, which the event envelope carries in `resources`.
    pub job_arn: String,
    /// The `JobStatus` the job moved INTO, kept beside the detail so a caller
    /// (and a test) can see which transition this record is for without parsing.
    pub status: String,
    /// WHEN the job moved, in epoch milliseconds: the event's `time`, so the
    /// envelope reports the transition's instant rather than the instant the queue
    /// was drained (which a reader can materialise much later).
    pub at: i64,
    /// The rendered `detail` of the event: the job's own description.
    pub detail: ::serde_json::Value,
}

/// The `detail` of a `Batch Job State Change` event: the job as this store holds
/// it, in the member names the `JobDetail` shape documents.
///
/// "The `detail` section of the event contains the job's details, in the same
/// form as the `DescribeJobs` API returns them" ("AWS Batch events"), so every
/// member below is read from the stored record — the SAME record `DescribeJobs`
/// projects — and nothing that is not stored is invented: `attempts` reports the
/// attempts the record holds (empty for a job nothing has placed, as the sample
/// event shows it), and `startedAt`, `stoppedAt`, `statusReason` and the optional
/// members appear only once the record carries them.
pub fn job_state_change_detail(job: &Job) -> ::serde_json::Value {
    use ::serde_json::{json, Map, Value};

    let mut detail = Map::new();
    detail.insert("jobArn".to_string(), json!(job.job_arn));
    detail.insert("jobName".to_string(), json!(job.job_name));
    detail.insert("jobId".to_string(), json!(job.job_id));
    detail.insert("jobQueue".to_string(), json!(job.job_queue_arn));
    detail.insert("jobDefinition".to_string(), json!(job.job_definition_arn));
    detail.insert("status".to_string(), json!(job.status));
    if let Some(reason) = &job.status_reason {
        detail.insert("statusReason".to_string(), json!(reason));
    }
    detail.insert("createdAt".to_string(), json!(job.created_at));
    if let Some(at) = job.started_at {
        detail.insert("startedAt".to_string(), json!(at));
    }
    if let Some(at) = job.stopped_at {
        detail.insert("stoppedAt".to_string(), json!(at));
    }
    // One entry per attempt the record holds, present and empty for a job no placement
    // has started an attempt for (the "AWS Batch events" sample event shows
    // `"attempts": []`).
    detail.insert(
        "attempts".to_string(),
        Value::Array(job.attempts.iter().map(attempt_event_detail).collect()),
    );
    detail.insert(
        "dependsOn".to_string(),
        Value::Array(
            job.depends_on
                .iter()
                .flatten()
                .map(|dependency| {
                    let mut entry = Map::new();
                    if let Some(id) = &dependency.job_id {
                        entry.insert("jobId".to_string(), json!(id));
                    }
                    if let Some(kind) = &dependency.r#type {
                        entry.insert("type".to_string(), json!(kind));
                    }
                    Value::Object(entry)
                })
                .collect(),
        ),
    );
    detail.insert(
        "parameters".to_string(),
        json!(job.parameters.clone().unwrap_or_default()),
    );
    if let Some(container) = &job.container {
        detail.insert("container".to_string(), container_event_detail(container));
    }
    if let Some(identifier) = &job.share_identifier {
        detail.insert("shareIdentifier".to_string(), json!(identifier));
    }
    if let Some(priority) = job.scheduling_priority {
        detail.insert("schedulingPriority".to_string(), json!(priority));
    }
    if let Some(timeout) = &job.timeout {
        if let Some(seconds) = timeout.attempt_duration_seconds {
            detail.insert(
                "timeout".to_string(),
                json!({ "attemptDurationSeconds": seconds }),
            );
        }
    }
    if let Some(strategy) = &job.retry_strategy {
        let mut rendered = Map::new();
        if let Some(attempts) = strategy.attempts {
            rendered.insert("attempts".to_string(), json!(attempts));
        }
        detail.insert("retryStrategy".to_string(), Value::Object(rendered));
    }
    if let Some(capabilities) = &job.platform_capabilities {
        detail.insert("platformCapabilities".to_string(), json!(capabilities));
    }
    if let Some(propagate) = job.propagate_tags {
        detail.insert("propagateTags".to_string(), json!(propagate));
    }
    if let Some(tags) = &job.tags {
        detail.insert("tags".to_string(), json!(tags));
    }
    // The array properties the RECORD holds: a child's own index, a parent's size. The
    // parent's `statusSummary` is counted from the children's records and so is not a
    // member of the job this detail is about; it is reported by the readers that can see
    // them (`DescribeJobs`, `ListJobs`).
    if let Some(index) = job.array_index {
        detail.insert("arrayProperties".to_string(), json!({ "index": index }));
    } else if let Some(size) = job.array_size {
        detail.insert("arrayProperties".to_string(), json!({ "size": size }));
    }
    detail.insert("isCancelled".to_string(), json!(job.is_cancelled));
    detail.insert("isTerminated".to_string(), json!(job.is_terminated));
    Value::Object(detail)
}

/// The `container` member of a job state change's detail: the stored effective
/// container, in the member names `ContainerDetail` documents.
///
/// Only the members this store actually holds are rendered — the effective
/// container the definition and the request's overrides add up to, beside what the run of
/// its declared program produced ([`super::container_run`]): the status that program exited
/// with and the task and log stream the placement named. A member no run produced is left
/// out rather than invented, so a container nothing ran reports no `exitCode` and a
/// container whose output went to another log driver reports no `logStreamName`.
fn container_event_detail(container: &ContainerDetail) -> ::serde_json::Value {
    use ::serde_json::{json, Map, Value};

    let mut rendered = Map::new();
    if let Some(image) = &container.image {
        rendered.insert("image".to_string(), json!(image));
    }
    if let Some(vcpus) = container.vcpus {
        rendered.insert("vcpus".to_string(), json!(vcpus));
    }
    if let Some(memory) = container.memory {
        rendered.insert("memory".to_string(), json!(memory));
    }
    if let Some(command) = &container.command {
        rendered.insert("command".to_string(), json!(command));
    }
    if let Some(role) = &container.job_role_arn {
        rendered.insert("jobRoleArn".to_string(), json!(role));
    }
    if let Some(role) = &container.execution_role_arn {
        rendered.insert("executionRoleArn".to_string(), json!(role));
    }
    if let Some(user) = &container.user {
        rendered.insert("user".to_string(), json!(user));
    }
    if let Some(privileged) = container.privileged {
        rendered.insert("privileged".to_string(), json!(privileged));
    }
    if let Some(readonly) = container.readonly_root_filesystem {
        rendered.insert("readonlyRootFilesystem".to_string(), json!(readonly));
    }
    if let Some(reason) = &container.reason {
        rendered.insert("reason".to_string(), json!(reason));
    }
    if let Some(code) = container.exit_code {
        rendered.insert("exitCode".to_string(), json!(code));
    }
    if let Some(task_arn) = &container.task_arn {
        rendered.insert("taskArn".to_string(), json!(task_arn));
    }
    if let Some(stream) = &container.log_stream_name {
        rendered.insert("logStreamName".to_string(), json!(stream));
    }
    if let Some(environment) = &container.environment {
        rendered.insert(
            "environment".to_string(),
            Value::Array(
                environment
                    .iter()
                    .map(|pair| {
                        let mut entry = Map::new();
                        if let Some(name) = &pair.name {
                            entry.insert("name".to_string(), json!(name));
                        }
                        if let Some(value) = &pair.value {
                            entry.insert("value".to_string(), json!(value));
                        }
                        Value::Object(entry)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(requirements) = &container.resource_requirements {
        rendered.insert(
            "resourceRequirements".to_string(),
            Value::Array(
                requirements
                    .iter()
                    .map(|requirement| {
                        json!({ "type": requirement.r#type, "value": requirement.value })
                    })
                    .collect(),
            ),
        );
    }
    Value::Object(rendered)
}
