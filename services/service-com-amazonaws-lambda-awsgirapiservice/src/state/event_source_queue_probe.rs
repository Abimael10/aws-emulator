// SPDX-License-Identifier: CC0-1.0

//! Whether the QUEUE an event source mapping would poll is a queue the queue service
//! answers for — asked of that service itself.
//!
//! A mapping onto an Amazon SQS queue is wired up by RECEIVING from the queue its
//! `EventSourceArn` addresses, so what decides such a mapping is the answer the queue
//! service gives that read rather than anything the ARN's fields look like: a queue nothing
//! holds answers the read with its own absence, and Lambda reports that failure as the read
//! it came from ([`super::propagated_queue_read_failure`]), which is the only refusal that
//! tells the caller to go create the queue. The sibling decisions over the same member are
//! the ones taken on the value alone ([`super::event_source_request`]); this one needs the
//! queue service.
//!
//! So the question is put to that service over the in-process callback into this same
//! emulator, exactly as the mapping's own poller reaches the services it reads and writes
//! ([`super::event_source_poller`]): the queue is RESOLVED BY NAME, because the decision is
//! whether the queue is there and a probe may not take a message that a real consumer would
//! then not see — the resolution answers the same absence the receive would. Two answers
//! therefore show nothing and refuse nothing: an emulator that serves no queue service at
//! all, where the call never reaches a handler, and any other failure of the call (a read
//! this caller may not make, a throttle), which is not the queue's absence.
//!
//! A READ: it decides, and changes nothing in either service's store.

use ::service_com_amazonaws_sqs_amazonsqs as sqs;

/// Why the queue `event_source_arn` names cannot be received from because it is not there —
/// `None` for a value that names no queue, for a queue the queue service holds, and for every
/// answer that shows nothing about the queue.
///
/// `event_source_arn` is the source the mapping would poll, `None` for the self-managed
/// cluster, which is no queue.
///
/// The sentence is the queue service's own refusal of that read, propagated as Lambda reports
/// a failed source read: its error code and its message, as that service answers them to any
/// client ([`sqs::state::QUEUE_DOES_NOT_EXIST_CODE`],
/// [`sqs::state::QUEUE_DOES_NOT_EXIST_MESSAGE`]), so a caller branches on the queue service's
/// own code.
pub async fn absent_queue_event_source_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    event_source_arn: Option<&str>,
) -> Option<String> {
    let arn = ::service_common::arn::parse(event_source_arn?)?;
    if arn.vendor != super::EVENT_SOURCE_VENDOR_QUEUE {
        return None;
    }
    let refused = queue_client(ctx, arn.region)
        .get_queue_url()
        .queue_name(arn.resource)
        .queue_owner_aws_account_id(arn.account)
        .send()
        .await
        .err()?;
    // A failure that is no answer from the queue service — no service of this emulator
    // served the call at all — shows nothing about the queue, so it refuses nothing.
    refused
        .as_service_error()?
        .is_queue_does_not_exist()
        .then(|| {
            super::propagated_queue_read_failure(
                sqs::state::QUEUE_DOES_NOT_EXIST_CODE,
                sqs::state::QUEUE_DOES_NOT_EXIST_MESSAGE,
            )
        })
}

/// The Amazon SQS client this question is put through: the in-process callback into this
/// same emulator, against the queue service's endpoint in `region` — the Region the queue's
/// own ARN names, since a queue belongs to one Region's namespace.
///
/// Built the way every client this service reaches another service of this emulator through
/// is built (`super::event_source_poller`'s own slots): the endpoint url names the called
/// service's public host so the request is signed and routed as that service's, while the
/// transport is the emulator's own connector, so no socket is opened and no name is
/// resolved. Everything a network client needs and this one cannot have — retries, timeouts,
/// identity caching and stalled-stream protection, each of which needs a sleep
/// implementation the callback transport is not given — is switched off.
fn queue_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_sqs::Client {
    let config = ::aws_sdk_sqs::Config::builder()
        .behavior_version(::aws_sdk_sqs::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_sqs::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://sqs.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_sqs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_sqs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_sqs::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_sqs::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_sqs::Client::from_conf(config)
}
