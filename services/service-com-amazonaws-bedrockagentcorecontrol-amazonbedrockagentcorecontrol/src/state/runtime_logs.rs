// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs group this service VENDS for an agent runtime.
//!
//! An agent runtime's output is a MANAGED log destination: the service — not the client —
//! owns the group the runtime's logs land in, so from a served create onward a group named
//! for the runtime endpoint whose invocations land in it exists in the caller's account,
//! and a client that reads the runtime and then tails it (`logs:DescribeLogGroups`,
//! `logs:FilterLogEvents`) finds it there. Nothing in either create's output names it: the
//! name is DERIVED from the `agentRuntimeId` the create minted and the endpoint the
//! invocations are addressed to ([`runtime_log_group`]), which is why every client, console
//! view and troubleshooting workflow can address a runtime's logs without being told where
//! they are.
//!
//! An endpoint is what an invocation is addressed to, so the destination is per ENDPOINT:
//! `CreateAgentRuntime` establishes the runtime's default endpoint and vends that
//! endpoint's group, and a `CreateAgentRuntimeEndpoint` vends the group of the endpoint it
//! adds — one group per endpoint whose logs can be read, all under the one runtime's id.
//!
//! Three properties the group is created with, and one the delete has to respect:
//!
//! * the NAME is the runtimes prefix, the minted `agentRuntimeId` and the endpoint's name
//!   ([`runtime_log_group`]) — both variable parts are the resource's own, so the name is a
//!   function of the resource rather than of anything else the caller sent;
//! * NO RETENTION is configured, so the events never expire. CloudWatch Logs reports
//!   `retentionInDays` only for a group a retention policy was put on
//!   (`logs:PutRetentionPolicy`), and this create puts none, so a read of the group
//!   carries no such member at all;
//! * the group is created EMPTY: a stream and its events belong to whatever later writes
//!   to the runtime's logs, never to the create.
//! * the group is the ACCOUNT's, not the runtime's or the endpoint's child, so no delete in
//!   this service removes it or anything in it — the logs of a runtime that is gone are
//!   exactly what an operator diagnosing its last session reads. `DeleteAgentRuntime`
//!   therefore touches CloudWatch Logs nowhere.
//!
//! The group is provisioned by calling the emulator's OWN CloudWatch Logs service over the
//! in-process callback ([`smithy_server_runtime::OperationContext::callback`]), the way
//! this crate already resolves object storage and IAM, so no other service crate is named
//! here. Two consequences, both deliberate:
//!
//! * it is BEST EFFORT. A router that mounts this service WITHOUT CloudWatch Logs cannot
//!   provision anything, and its answer — the router's own `UnknownService` — says nothing
//!   about the caller's request, which asked for a runtime and never for a log group. So a
//!   failure to provision is traced rather than failing a create whose runtime is already
//!   recorded; the assembled emulator mounts every service crate in the workspace, and
//!   this crate's own suite mounts the pair explicitly;
//! * it is IDEMPOTENT. A group already under that name is the group this runtime's logs
//!   land in, so finding one is success: a replayed create (one carrying a `clientToken`
//!   that was already served) and a re-created id both adopt it instead of colliding.

/// The prefix CloudWatch Logs holds an AgentCore Runtime's vended logs under. Fixed: the
/// service vends into its own namespace, and the caller names no part of it.
pub const RUNTIME_LOG_GROUP_PREFIX: &str = "/aws/bedrock-agentcore/runtimes/";

/// The endpoint whose logs the group vended by `CreateAgentRuntime` carries: the default
/// endpoint every agent runtime is invocable at, which is the one the runtime's create
/// itself establishes. A caller-named endpoint carries its own name instead.
pub const DEFAULT_RUNTIME_ENDPOINT: &str = "DEFAULT";

/// The log group the logs of one agent runtime ENDPOINT are vended into: the runtimes
/// prefix, the runtime's own id, and the name of the endpoint whose invocations land there.
///
/// Every character of it is one CloudWatch Logs admits in a group name
/// (`^[\.\-_/#A-Za-z0-9]+$`, `smithy_query(query="shape", shape="LogGroupName",
/// model_path="services/service-com-amazonaws-cloudwatchlogs-logs-20140328/smithy-model/\
/// com-amazonaws-cloudwatchlogs-logs-20140328.json")`): an `agentRuntimeId` is
/// `^[a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}$` and an `EndpointName` is
/// `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`, and the longest pair they admit still leaves the name
/// well inside that shape's 1..512 length.
pub fn runtime_log_group(agent_runtime_id: &str, endpoint_name: &str) -> String {
    format!("{RUNTIME_LOG_GROUP_PREFIX}{agent_runtime_id}-{endpoint_name}")
}

/// Create the log group this service vends for the `endpoint_name` endpoint of the agent
/// runtime `agent_runtime_id`, so the group a client derives from the create's own answer
/// is a group CloudWatch Logs reports.
///
/// Created with NO retention policy, which is what makes its events never expire.
///
/// Best effort: a group that is already there is success, and anything else is traced. See
/// this module's header. Call it with NO state guard held — it awaits.
pub async fn vend_runtime_log_group(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    agent_runtime_id: &str,
    endpoint_name: &str,
) {
    let log_group = runtime_log_group(agent_runtime_id, endpoint_name);
    match logs_client(ctx, region)
        .create_log_group()
        .log_group_name(&log_group)
        .send()
        .await
    {
        Ok(_) => {}
        // The group this runtime's logs land in is there, which is all this step owes.
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service| service.is_resource_already_exists_exception()) => {}
        Err(error) => {
            ::tracing::warn!(
                %agent_runtime_id,
                %endpoint_name,
                %log_group,
                %region,
                detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the agent runtime's vended log group could not be created, so a client that \
                 tails the runtime will not find it",
            );
        }
    }
}

/// The CloudWatch Logs client this service vends a runtime's group through: the in-process
/// callback into this same emulator, at the endpoint of the REQUEST's own Region, so the
/// group lands where the runtime's ARN says the runtime is.
fn logs_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_cloudwatchlogs::Client {
    let config = ::aws_sdk_cloudwatchlogs::Config::builder()
        .behavior_version(::aws_sdk_cloudwatchlogs::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_cloudwatchlogs::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://logs.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client
        // is not given. The same goes for caching the identity: the credentials are handed
        // over directly.
        .retry_config(::aws_sdk_cloudwatchlogs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudwatchlogs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudwatchlogs::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudwatchlogs::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudwatchlogs::Client::from_conf(config)
}
