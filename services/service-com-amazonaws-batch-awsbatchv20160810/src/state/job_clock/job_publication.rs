// SPDX-License-Identifier: CC0-1.0

//! PUBLISHING what a job transition produced: the side effects one drain settles, and the
//! job state changes it puts on the account's default EventBridge bus.
//!
//! Every one of them is a call to ANOTHER service, so each is made with this store's lock
//! released, in the order the records they write are needed in. The drain is reached from a
//! handler ([`publish_job_state_changes`]) and from the clock pump ([`super::drive_job_clock`])
//! through the one function below, so a transition the clock materialises leaves exactly the
//! same side effects as one a caller did.

use super::*;

/// Publish every job state change this store has recorded onto the account's
/// default EventBridge bus, and leave the queue empty.
///
/// This is Batch's side of the documented, polling-free way to observe a job:
/// "AWS Batch sends job state change events to EventBridge" with
/// `source` [`EVENT_SOURCE`] and `detail-type`
/// [`JOB_STATE_CHANGE_DETAIL_TYPE`] ("AWS Batch events"), so a rule on the
/// default bus matching that pair reaches its targets. EventBridge is reached the
/// way any emulated service reaches another: through the in-process callback
/// ([`smithy_server_runtime::OperationContext::callback`]), so this crate names
/// no other service crate.
///
/// Called by a handler AFTER it has released this store's lock, so no lock is
/// held across the call. Nothing about the answer changes what the caller is
/// told: publication is "on a best-effort basis" ("AWS Batch events"), and an
/// emulator with no EventBridge mounted has nowhere to publish, so a failure is
/// logged and the operation's own result stands.
pub async fn publish_job_state_changes<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
    region: &str,
) {
    publish_pending(ctx.callback(), engine, region).await;
}

/// [`publish_job_state_changes`] over the callback alone, so the clock pump
/// ([`drive_job_clock`]) — which outlives the request that started it and has no
/// `OperationContext` — publishes through exactly the same path a handler does.
pub(super) async fn publish_pending<Eng: Send + Sync + 'static>(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    engine: &Eng,
    region: &str,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // The image fetch a job waiting to be placed needs, answered by the registry that holds
    // the image, with this store's lock released (see [`image_pull`]). It runs before the
    // streams and the events below, because the verdict it records is what the placement
    // ahead of it materialises.
    settle_job_image_fetches(engine);
    // The program the container of a job the scheduler has just PLACED runs, performed in
    // process with this store's lock released (see [`container_run`]) — for a job no Amazon
    // ECS task runs, which the step below is what decides. It runs after the fetch above,
    // because a job is only placed once its image has been answered for, and before the
    // streams and the events below, because the lines it wrote are delivered to the job's own
    // log stream.
    settle_job_container_runs(engine).await;
    // The clusters backing this store's compute environments, created and deleted where
    // Amazon ECS holds them, over the same callback (see [`ecs_cluster`]).
    settle_backing_clusters(callback, &state).await;
    // The TASKS the placed jobs run as, in those clusters and over that same callback: the
    // container of a placed job is a container of an Amazon ECS task, so the task is created
    // and read where that service holds it (see [`ecs_task`]). It runs before the streams and
    // the events below, because the placement a created task makes — and the end a stopped
    // one writes — are what those two carry.
    settle_job_tasks(callback, &state).await;
    // The other side effect a job transition leaves for this drain: the CloudWatch Logs
    // stream a placement named is created where that service holds it, over the same
    // callback and with this store's lock released (see [`job_logs`]).
    open_pending_log_streams(callback, &state).await;
    // The guard is taken and dropped inside this statement: the records are moved
    // out here so nothing is published twice, and no lock is held across an await.
    let changes = state.lock().take_pending_events(MAX_PUBLISHED_PER_DRAIN);
    if changes.is_empty() {
        return;
    }
    let config = ::aws_sdk_eventbridge::Config::builder()
        .behavior_version(::aws_sdk_eventbridge::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_eventbridge::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://events.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or
        // watch for a stalled stream, and each of those would need a sleep
        // implementation this client is not given. The same goes for caching the
        // identity: the credentials are handed over directly.
        .retry_config(::aws_sdk_eventbridge::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_eventbridge::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_eventbridge::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_eventbridge::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    let client = ::aws_sdk_eventbridge::Client::from_conf(config);
    for window in changes.chunks(MAX_PUT_EVENTS_ENTRIES) {
        let mut request = client.put_events();
        for change in window {
            let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
                .event_bus_name(DEFAULT_EVENT_BUS)
                .source(EVENT_SOURCE)
                .detail_type(JOB_STATE_CHANGE_DETAIL_TYPE)
                // The event's `time` is WHEN the job moved, not when this queue was
                // drained: `PutEventsRequestEntry$Time` is "the time stamp of the
                // event, per RFC3339. If no time stamp is provided, the time stamp
                // of the PutEvents call is used."
                .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
                    change.at,
                ))
                // "resources: the job's ARN" — the event names the resource it is
                // about, which is what a rule filtering on `resources` reads.
                .resources(&change.job_arn)
                .detail(change.detail.to_string())
                .build();
            request = request.entries(entry);
        }
        if let Err(error) = request.send().await {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a Batch job state change could not be published to EventBridge"
            );
        }
    }
}
