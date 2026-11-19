// SPDX-License-Identifier: CC0-1.0

//! Where a notebook instance's lifecycle-configuration hook output GOES: the CloudWatch
//! Logs group and stream the platform publishes it under, and the publication itself.
//!
//! The hook shape's own doc names both: "View Amazon CloudWatch Logs for notebook instance
//! lifecycle configurations in log group `/aws/sagemaker/NotebookInstances` in log stream
//! `[notebook-instance-name]/[LifecycleConfigHook]`" — so a client reads what a hook wrote
//! through CloudWatch Logs' own API rather than off the notebook instance, and the group,
//! the stream and the events are that service's resources.
//!
//! What the hook wrote is computed by [`super::notebook_lifecycle_hook`]; this module only
//! delivers it, through the ONE publication path this crate's job and hook logs share
//! ([`super::published_container_logs`]) — which is also the seam that reports a delivery
//! this emulator could not make. The delivery is BEST EFFORT: an emulator with no CloudWatch
//! Logs mounted beside SageMaker, or a write CloudWatch Logs refuses, is reported in this
//! service's own tracing rather than failing the notebook-instance call, which asked for an
//! instance and has it stored either way. Nothing here deletes a group or a stream: the log
//! records in it belong to the account.

use super::*;

/// Everything one hook publication needs, read while the store's lock is held so the
/// publication itself runs with none.
#[derive(Debug, Clone)]
pub struct HookLogPublication {
    /// The notebook instance the hook ran for, which names the log stream.
    pub notebook_instance_name: String,
    /// Which of the configuration's scripts ran, which names the rest of the stream.
    pub hook: LifecycleHook,
    /// The lines the script wrote, in the order it wrote them.
    pub lines: Vec<String>,
}

/// Publish what a hook wrote to the stream the hook shape documents for it: create the
/// notebook-instance log group if CloudWatch Logs does not hold it yet, open this hook's
/// stream, and deliver one log event per line the script wrote.
///
/// A hook that wrote nothing publishes nothing — there is no record to deliver, and a stream
/// opened for it would report output where there was none.
///
/// Best effort and never fatal — see this module's header. Call it with NO store guard
/// held: it awaits.
pub async fn publish_hook_output(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    publication: &HookLogPublication,
) {
    if publication.lines.is_empty() {
        return;
    }
    let log_stream = publication
        .hook
        .log_stream_name(&publication.notebook_instance_name);
    let published = published_container_logs(
        ctx,
        region,
        NOTEBOOK_LOG_GROUP,
        &log_stream,
        &publication.lines,
    )
    .await;
    if !published {
        ::tracing::warn!(
            notebook_instance_name = ?publication.notebook_instance_name,
            log_group = %NOTEBOOK_LOG_GROUP,
            log_stream = ?log_stream,
            "this lifecycle hook's output is not published, so nothing can read it"
        );
    }
}
