// SPDX-License-Identifier: CC0-1.0

//! The Amazon SNS topic an assistant streams chat messages through, provisioned in and
//! resolved against the service that OWNS topics.
//!
//! `AssistantData.integrationConfiguration.topicIntegrationArn` is "the Amazon Resource Name
//! (ARN) of the integrated Amazon SNS topic used for streaming chat messages"
//! (`smithy_query(query="shape",shape="AssistantIntegrationConfiguration")`), and
//! `CreateAssistant` models no member for it: the service provisions the topic with the
//! assistant and reports it. What it names is an Amazon SNS resource, so this service does
//! not get to compose the ARN of one: the topic is CREATED in Amazon SNS and the ARN that
//! service answered is what the assistant reports. Amazon SNS is reached the way this
//! emulator reaches any other service it mounts — the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]) — so nothing leaves this process.
//!
//! A hop that could not be made at all — no Amazon SNS in this process, a Region label that
//! names no endpoint to address, an answer that could not be read — leaves the assistant with
//! NO topic, and it reports none: an ARN that resolves to no topic would promise a stream a
//! client cannot subscribe to.

/// The name of the Amazon SNS topic the assistant `assistant_id` streams chat messages
/// through: the topic belongs to that assistant, so it is named after it.
///
/// It satisfies the topic-name rule Amazon SNS publishes (alphanumerics, hyphens and
/// underscores, at most 256 characters), since an `assistantId` is a `Uuid`. Internal to this
/// module: what leaves it is the ARN Amazon SNS answered, never a name this service composed.
fn streaming_topic_name(assistant_id: &str) -> String {
    format!("qconnect-assistant-{assistant_id}")
}

/// Provision the streaming topic of the assistant `assistant_id` in `region`, answering the
/// ARN Amazon SNS reports for it. `None` when Amazon SNS did not answer with one, in which
/// case the assistant is integrated with no topic.
///
/// `CreateTopic` "is idempotent, so if the requester already owns a topic with the specified
/// name, that topic's ARN is returned without creating a new topic", so asking twice for one
/// assistant's topic answers the one topic rather than a second one.
pub async fn provision_assistant_streaming_topic(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    assistant_id: &str,
) -> Option<String> {
    let client = sns(ctx, region)?;
    let name = streaming_topic_name(assistant_id);
    match client.create_topic().name(&name).send().await {
        Ok(answer) => answer.topic_arn,
        Err(error) => {
            // At `debug`: in an emulator carrying no Amazon SNS this is the normal answer,
            // and what a caller sees — an assistant reporting no integrated topic — already
            // says the assistant has none.
            ::tracing::debug!(
                topic = %name,
                region = %region,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the streaming topic of this assistant could not be created in Amazon SNS"
            );
            None
        }
    }
}

/// Release the streaming topic `topic_arn`, which was provisioned with an assistant that has
/// just been deleted, so the account is not left holding the topic of an assistant that no
/// longer exists.
///
/// A failure is LOGGED, not raised: the topic is the service's own, the assistant is already
/// gone from this store, and nothing a caller asked for depends on the release.
pub async fn release_assistant_streaming_topic(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    topic_arn: &str,
) {
    let Some(client) = sns(ctx, region) else {
        return;
    };
    if let Err(error) = client.delete_topic().topic_arn(topic_arn).send().await {
        ::tracing::debug!(
            topic = %topic_arn,
            region = %region,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the streaming topic of the deleted assistant could not be released in Amazon SNS"
        );
    }
}

/// An AWS SDK client for Amazon SNS over the in-process callback into this same emulator,
/// addressed in `region`.
///
/// The endpoint URL is only the address the callback's transport routes on IN PROCESS: no
/// socket is opened, no name is resolved and nothing leaves this process. The credentials are
/// the callback's, which sign this in-process hop and never appear in a response. Nothing
/// else an SDK client does has anything to do here: no retries, no timeouts and no identity
/// cache.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in an
/// endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, the one place this tree states that grammar) instead
/// of being interpolated as given: `None` for a value that would name an authority the caller
/// chose rather than the peer service this emulator asks.
fn sns(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_sns::Client> {
    let region = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_sns::Config::builder()
        .behavior_version(::aws_sdk_sns::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_sns::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://sns.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_sns::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_sns::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_sns::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_sns::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_sns::Client::from_conf(config))
}
