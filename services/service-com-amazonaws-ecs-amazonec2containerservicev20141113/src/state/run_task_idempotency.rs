// SPDX-License-Identifier: CC0-1.0

//! Whether a `RunTask` that carries a `clientToken` is a run this store has ALREADY
//! performed.
//!
//! `RunTask$clientToken` is "An identifier that you provide to ensure the idempotency of
//! the request", so the run it names is performed at most once: the first call places the
//! tasks and the token is recorded against them, and a later call carrying that same token
//! is that same run being made again rather than a second one. Which answer the repeat
//! earns is decided by whether it asks for the SAME run:
//!
//! * the same arguments — the tasks the first call created are reported back, so a client
//!   that retried (an SDK retry, a re-run of a pipeline step) holds one task and not two;
//! * different arguments — the token is in use for another run, which the operation's own
//!   doc states as the conflict it is: "The provided `clientToken` is already in use with a
//!   different `RunTask` request. The `resourceIds` are the existing task ARNs which are
//!   already associated with the `clientToken`." Nothing is placed and nothing is
//!   rewritten.
//!
//! The token is recorded per CLUSTER of a Region: a `RunTask` places tasks into one
//! cluster, and the ARNs the record holds are that cluster's, so the same token names
//! another run in another cluster and in another Region.
//!
//! The ARGUMENTS are compared as the request sent them, apart from the three the handler
//! resolves before it reaches this point: `clientToken` itself (the key), `cluster` (the
//! cluster the key is scoped to, so naming it by name or by ARN is the same run) and
//! `count` (the member's documented default, so an absent count and an explicit `1` are the
//! same run). The same shape as the other repeats this crate recognises —
//! [`RequestedClusterArguments`](super::RequestedClusterArguments) does it for a cluster
//! create, [`capacity_provider_binding_fingerprint`](super::capacity_provider_binding_fingerprint)
//! for a capacity provider's binding.

use std::time::SystemTime;

use super::{ComAmazonawsEcsAmazonec2containerservicev20141113State, Task};
use crate::ops::types::RunTaskInput;

/// `ConflictException$message` for a `RunTask` whose `clientToken` is already in use with a
/// different run.
///
/// The recorded refusal is exactly these two sentences, and the operation's own doc states
/// the same condition: "the `RunTask` request could not be processed due to conflicts. The
/// provided `clientToken` is already in use with a different `RunTask` request. The
/// `resourceIds` are the existing task ARNs which are already associated with the
/// `clientToken`." What Amazon ECS answers names the conflicting call as a request rather
/// than by the operation, and leaves the ARNs at issue to `resourceIds` rather than putting
/// them in the sentence.
pub const RUN_TASK_CLIENT_TOKEN_CONFLICT_MESSAGE: &str =
    "The RunTask request could not be processed due to conflicts. The provided clientToken \
     is already in use with a different request.";

/// The most `clientToken` records this store keeps. A token is remembered so a RETRY of its
/// run is recognised, and the retries a client makes are of its recent calls, so the bound
/// is held by dropping the record whose run was placed longest ago.
///
/// What one record holds is bounded with it: the task ARNs of one run (`RunTask` places at
/// most [`MAX_RUN_TASK_COUNT`](super::MAX_RUN_TASK_COUNT) tasks) and the arguments of one
/// request, whose size is the request body's.
pub const MAX_RUN_TASK_CLIENT_TOKENS: usize = 1_024;

/// What one `clientToken` of one cluster names: `(region, clusterName, clientToken)`.
pub type RunTaskTokenKey = (String, String, String);

/// The run one `clientToken` was used for: the arguments it asked for and the tasks it
/// placed.
#[derive(Debug, Clone)]
pub struct RunTaskTokenRecord {
    /// The arguments of the run, as [`run_task_arguments`] renders them, so a repeat can be
    /// compared against what the token was used for.
    pub arguments: String,
    /// The tasks that run placed, in the order it placed them — the ARNs a repeat is
    /// answered with and the `resourceIds` a conflict reports.
    pub task_arns: Vec<String>,
    /// When the run was placed, which is what the bound above drops the oldest of.
    pub recorded_at: SystemTime,
}

/// What a `clientToken` this store already holds means for the run carrying it.
#[derive(Debug)]
pub enum RunTaskReplay {
    /// The repeat asks for the run the token was used for, so it is answered with that
    /// run's tasks, read back out of the store as they stand now.
    Tasks(Vec<Task>),
    /// The repeat asks for something else, so it is refused with the task ARNs the token is
    /// already associated with.
    Conflict(Vec<String>),
}

/// The ARGUMENTS one `RunTask` asks for, rendered as the string two runs under one token
/// are compared on.
///
/// Everything the request carries takes part except the three members resolved before the
/// comparison: `clientToken`, which is the key rather than an argument, and `cluster` and
/// `count`, which are passed in as the handler resolved them so that two spellings of one
/// cluster, and an absent `count` beside the `1` it defaults to, are the same run.
pub fn run_task_arguments(input: &RunTaskInput, cluster_name: &str, count: i32) -> String {
    let mut arguments = input.clone();
    arguments.client_token = None;
    arguments.cluster = Some(cluster_name.to_string());
    arguments.count = Some(count);
    format!("{arguments:?}")
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// What the `clientToken` of `key` means for a run asking for `arguments`, or `None`
    /// when this store has no run recorded under it — the run that goes ahead.
    ///
    /// A record whose tasks this store no longer holds answers `None` too: there is no run
    /// to report back, so the token is free for the run that carries it.
    pub fn run_task_replay(
        &self,
        key: &RunTaskTokenKey,
        arguments: &str,
    ) -> Option<RunTaskReplay> {
        let record = self.run_task_client_tokens.get(key)?;
        if record.arguments != arguments {
            return Some(RunTaskReplay::Conflict(record.task_arns.clone()));
        }
        // Reported from the stored tasks rather than from anything kept on the record, so
        // the repeat answers the tasks as they stand now — the statuses the clock has moved
        // them to since. `RunTask` has no `include` member to narrow a task with, so the
        // tags it was created with are reported, exactly as the first call reported them.
        let tasks: Vec<Task> = record
            .task_arns
            .iter()
            .filter_map(|arn| self.tasks.get(arn))
            .map(|task| task.view(true))
            .collect();
        (!tasks.is_empty()).then_some(RunTaskReplay::Tasks(tasks))
    }

    /// Record that the run `key`'s token names placed `task_arns`, asking for `arguments`,
    /// at `at`.
    ///
    /// Bounded by [`MAX_RUN_TASK_CLIENT_TOKENS`]: once the bound is reached the record whose
    /// run was placed longest ago is dropped, because the newest runs are the ones a client
    /// is still retrying.
    pub fn record_run_task_token(
        &mut self,
        key: RunTaskTokenKey,
        arguments: String,
        task_arns: Vec<String>,
        at: SystemTime,
    ) {
        if task_arns.is_empty() {
            return;
        }
        self.run_task_client_tokens.insert(
            key,
            RunTaskTokenRecord {
                arguments,
                task_arns,
                recorded_at: at,
            },
        );
        while self.run_task_client_tokens.len() > MAX_RUN_TASK_CLIENT_TOKENS {
            let Some(oldest) = self
                .run_task_client_tokens
                .iter()
                .min_by_key(|(key, record)| (record.recorded_at, (*key).clone()))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.run_task_client_tokens.remove(&oldest);
        }
    }
}
