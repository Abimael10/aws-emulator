// SPDX-License-Identifier: CC0-1.0

//! The DELIVERY a subscriber owes its target: the call that puts an accepted
//! event where the subscriber's `InvokeConfiguration` names.
//!
//! `CreateSubscriber` creates "a subscriber on an event bus, which delivers
//! matching events to the configured target", so a bus that accepts an event
//! owes that event to each of its subscribers. Nothing a client calls makes that
//! happen: a client publishes and then watches the TARGET, which is why the
//! publish itself performs the deliveries it has just made due.
//!
//! Which subscriber is owed an event, and what its target is handed, is
//! [`super::subscribers`]; this module is the call alone. It is made over the
//! IN-PROCESS callback into this same emulator
//! (`OperationContext::callback`) — no socket is opened and nothing leaves the
//! host — so what the target holds afterwards is what that service's own
//! handlers wrote.
//!
//! The target kinds this slice does not call are recorded as NOT ATTEMPTED and
//! never as delivered: a delivery nobody made must not be reported as one.
//!
//! Two things decide a delivery beside the subscriber's filters. The ROLE it is
//! made under — "IAM role the service assumes to invoke the target"
//! (`InvokeConfiguration$RoleArn`) — so the permissions in force on that role
//! decide the call before it is made ([`delivery_authorization`]), whatever the
//! publisher of the event may do. And a delivery that does not happen leaves a
//! record where the subscriber's `OnFailureConfiguration` names — "where a
//! failed delivery is sent" ([`dead_letter`]) — so a client sees the failure
//! even though the target holds nothing.

mod dead_letter;
mod delivery_authorization;
mod topic_target;

use super::subscribers::{delivered_body, delivered_event, filters_match, DueDelivery, Subscriber};
use dead_letter::Failure;

/// The `arnNamespace` of Amazon SQS, the vendor field of a queue's ARN.
pub const SQS_ARN_NAMESPACE: &str = "sqs";

/// The `arnNamespace` of Amazon SNS, the vendor field of a topic's ARN.
pub const SNS_ARN_NAMESPACE: &str = "sns";

/// The IAM action the call a delivery to a QUEUE makes performs: `SendMessage`
/// is the action of the `SendMessageBatch` a queue target is handed its events
/// by, as Amazon SQS publishes its actions.
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The IAM action the call a delivery to a TOPIC makes performs: `Publish` is
/// the action of the `PublishBatch` a topic target is handed its events by, as
/// Amazon SNS publishes its actions.
const PUBLISH_ACTION: &str = "sns:Publish";

/// How many messages one `SendMessageBatch` call carries at most.
///
/// "For Amazon SQS, the maximum number of entries you can include in a single
/// `SendMessageBatch` … request is 10"
/// (`com.amazonaws.sqs#TooManyEntriesInBatchRequest`), so a fan-out of more
/// events than that is sent as several calls rather than one the queue refuses.
pub const MAX_QUEUE_BATCH_ENTRIES: usize = 10;

/// The `DataType` a message attribute carrying text declares
/// (`com.amazonaws.sqs#MessageAttributeValue$DataType`: "String").
const STRING_ATTRIBUTE_TYPE: &str = "String";

/// The one message system attribute Amazon SQS accepts on a send
/// (`com.amazonaws.sqs#MessageSystemAttributeNameForSends`).
const TRACE_HEADER_ATTRIBUTE: &str = "AWSTraceHeader";

/// Deliver what each subscriber of a bus is owed — the events taken for it by
/// [`super::ComAmazonawsEventbridgev2Awseventsv2State::take_due_deliveries`], in
/// publication order.
///
/// Each subscriber is handed the events its own filters select
/// ([`filters_match`]); the rest of its due window is skipped, because a filter
/// is what the subscriber asked to be delivered. The matching of one call is
/// charged to ONE budget across every subscriber and event, so a request can
/// never buy more matching work than that budget sells.
pub async fn deliver_due(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    due: &[DueDelivery],
) {
    let mut budget = ::service_common::event_pattern::MatchBudget::new(
        ::service_common::event_pattern::MAX_MATCH_STEPS_PER_REQUEST,
    );
    for owed in due {
        let subscriber = &owed.subscriber;
        let mut bodies = Vec::new();
        for stored in &owed.events {
            let event = delivered_event(subscriber, stored);
            if !filters_match(subscriber.filter_configuration.as_ref(), &event, &mut budget) {
                continue;
            }
            match delivered_body(subscriber, &event) {
                Some(body) => bodies.push(Delivery {
                    event_id: event.event_id.clone(),
                    body,
                }),
                None => ::tracing::warn!(
                    subscriber = %subscriber.arn,
                    event = %event.event_id,
                    "an EventBridge subscriber was not delivered an event: its transformer names \
                     a transformation this emulator does not perform"
                ),
            }
        }
        if bodies.is_empty() {
            continue;
        }
        deliver_to_target(callback, subscriber, &bodies).await;
    }
}

/// One accepted event as the delivery hands it over: the text the target is
/// given, beside the identity of the event it carries — which is what a log or
/// a record of a delivery that did not happen names.
#[derive(Debug, Clone)]
struct Delivery {
    /// The `EventId` the publish reported for this event.
    event_id: String,
    /// The event in the shape the subscriber's `Transformer` names.
    body: String,
}

/// The resource a subscriber's `InvokeConfiguration` names, as the call that
/// hands it an event addresses it — and the resource an
/// `OnFailureConfiguration` names, which is a write of the same two kinds.
#[derive(Debug)]
enum Target {
    /// An Amazon SQS queue: each event travels as one message on it.
    Queue {
        /// The queue's ARN as it was configured: the resource the write is
        /// authorized on.
        arn: String,
        /// The url Amazon SQS addresses that queue by.
        url: String,
        /// The Region the ARN names — a delivery is made in the target's own
        /// Region.
        region: String,
    },
    /// An Amazon SNS topic: each event travels as one message published to it,
    /// which the topic's own subscriptions then carry.
    Topic {
        /// The topic's ARN as it was configured, which is both the resource the
        /// write is authorized on and what the publish addresses.
        arn: String,
        /// See [`Self::Queue::region`].
        region: String,
    },
}

/// Why a configured ARN addresses no resource a delivery can be made to. Each
/// is the clause a NOT ATTEMPTED log closes with, so a client reading the log
/// learns which part of the value was wrong.
const NAMES_NO_RESOURCE: &str = "it names no resource";
const NAMES_NO_QUEUE: &str = "it names no queue";
const NAMES_NO_TOPIC: &str = "it names no topic";
const NAMES_AN_UNCALLED_SERVICE: &str =
    "this emulator makes no call for a resource of that service yet";

impl Target {
    /// The resource `arn` names, or why it names none this delivery can reach.
    ///
    /// `bus_region` is the Region of the subscriber's own bus, which an ARN
    /// carrying no Region label addresses: every other value is reached in the
    /// Region its own ARN names, which is not necessarily the bus's.
    fn resolve(arn: &str, bus_region: &str) -> Result<Self, &'static str> {
        let Some(parsed) = ::service_common::arn::parse(arn) else {
            return Err(NAMES_NO_RESOURCE);
        };
        let region = ::service_common::arn::region_of(arn, parsed.vendor)
            .unwrap_or(bus_region)
            .to_string();
        match parsed.vendor {
            SQS_ARN_NAMESPACE => match queue_url(&region, parsed.account, parsed.resource) {
                Some(url) => Ok(Self::Queue {
                    arn: arn.to_string(),
                    url,
                    region,
                }),
                None => Err(NAMES_NO_QUEUE),
            },
            // A topic's ARN names the topic DIRECTLY
            // (`arn:<partition>:sns:<region>:<account>:<TopicName>`), so a
            // resource field carrying a further segment names a subscription or
            // a platform endpoint of that service rather than the topic itself.
            SNS_ARN_NAMESPACE => {
                let names_a_topic =
                    !parsed.resource.contains('/') && !parsed.resource.contains(':');
                if names_a_topic && !parsed.resource.is_empty() {
                    Ok(Self::Topic {
                        arn: arn.to_string(),
                        region,
                    })
                } else {
                    Err(NAMES_NO_TOPIC)
                }
            }
            _ => Err(NAMES_AN_UNCALLED_SERVICE),
        }
    }

    /// The ARN this resource was configured by: what a record of a failed
    /// delivery names, and the resource the write on it is authorized on.
    fn arn(&self) -> &str {
        match self {
            Self::Queue { arn, .. } | Self::Topic { arn, .. } => arn,
        }
    }

    /// The Region this resource is reached in.
    fn region(&self) -> &str {
        match self {
            Self::Queue { region, .. } | Self::Topic { region, .. } => region,
        }
    }

    /// The IAM action the call that hands this resource an event performs.
    fn action(&self) -> &'static str {
        match self {
            Self::Queue { .. } => SEND_MESSAGE_ACTION,
            Self::Topic { .. } => PUBLISH_ACTION,
        }
    }
}

/// Hand `bodies` to the resource this subscriber's `InvokeConfiguration` names,
/// through the one API that resource's service is invoked by, AS THE ROLE the
/// subscriber invokes its target with — and record what did not get there.
async fn deliver_to_target(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    subscriber: &Subscriber,
    bodies: &[Delivery],
) {
    let arn = subscriber.target_arn();
    let target = match Target::resolve(arn, &subscriber.region) {
        Ok(target) => target,
        Err(reason) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                target = %arn,
                "an EventBridge subscriber's delivery was not attempted: {reason}"
            );
            return;
        }
    };
    // The role decides the call BEFORE it is made: a role the permissions in
    // force on it do not let make that call is a delivery that does not happen,
    // so the target is handed nothing and every event of this window is
    // recorded as failed instead. WHICH document withheld it is reported to the
    // log, because the record a client reads states the condition rather than
    // the document.
    let undelivered: Vec<(&Delivery, Failure)> = match delivery_authorization::withheld(
        callback,
        &subscriber.region,
        &subscriber.invoke_configuration.role_arn,
        target.action(),
        target.arn(),
    )
    .await
    {
        Some(refusal) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                target = %target.arn(),
                %refusal,
                "an EventBridge subscriber's delivery was not made: the role it invokes its \
                 target with may not"
            );
            bodies
                .iter()
                .map(|delivery| (delivery, Failure::inaccessible(target.arn())))
                .collect()
        }
        None => match &target {
            Target::Queue { url, .. } => {
                send_to_queue(callback, subscriber, target.region(), url, target.arn(), bodies)
                    .await
            }
            Target::Topic { arn, .. } => {
                topic_target::publish(callback, subscriber, target.region(), arn, bodies).await
            }
        },
    };
    dead_letter::park(callback, subscriber, target.arn(), &undelivered).await;
}

/// Send one message per event to the queue at `queue_url`, in batches the queue
/// accepts, and report the deliveries it did not take.
///
/// The values the subscriber's `SqsParameters` carries travel with the send:
/// "Values are forwarded to the SQS `SendMessageBatch` API" (`SqsParameters`),
/// which is the API this delivery makes.
async fn send_to_queue<'a>(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    subscriber: &Subscriber,
    region: &str,
    queue_url: &str,
    queue_arn: &str,
    bodies: &'a [Delivery],
) -> Vec<(&'a Delivery, Failure)> {
    let client = sqs_client(callback, region);
    let parameters = subscriber.invoke_configuration.sqs_parameters.as_ref();
    let mut undelivered = Vec::new();
    for (batch, chunk) in bodies.chunks(MAX_QUEUE_BATCH_ENTRIES).enumerate() {
        let offset = batch * MAX_QUEUE_BATCH_ENTRIES;
        let mut request = client.send_message_batch().queue_url(queue_url);
        for (position, delivery) in chunk.iter().enumerate() {
            let mut entry = ::aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                // An entry id identifies one message WITHIN its batch
                // ("an identifier for a message in this batch used to
                // communicate the result"), and the character set it is held to
                // is alphanumerics, hyphens and underscores. It is this
                // delivery's position in the window, so what the queue reports
                // about an entry is read back onto the event it carried.
                .id((offset + position).to_string())
                .message_body(delivery.body.as_str());
            if let Some(parameters) = parameters {
                // A FIFO queue takes no message without a group, so the group
                // the subscriber names travels with every delivery to it.
                if let Some(group) = parameters.message_group_id.as_deref() {
                    entry = entry.message_group_id(group);
                }
                if let Some(deduplication_id) = parameters.message_deduplication_id.as_deref() {
                    entry = entry.message_deduplication_id(deduplication_id);
                }
                // `DelaySeconds` is text in this model, and the queue takes a
                // number: a value that is not one carries no delay rather than
                // one nobody asked for. `SqsParameters` is a @sensitive shape,
                // so what a refusal reports is the member and not its value.
                if let Some(delay) = parameters.delay_seconds.as_deref() {
                    match delay.parse::<i32>() {
                        Ok(seconds) => entry = entry.delay_seconds(seconds),
                        Err(error) => ::tracing::warn!(
                            subscriber = %subscriber.arn,
                            %error,
                            "an EventBridge subscriber's DelaySeconds is not a number of seconds \
                             and was not carried to its queue target"
                        ),
                    }
                }
                for (name, value) in parameters.message_attributes.iter().flatten() {
                    if let Some(attribute) = message_attribute(subscriber, value) {
                        entry = entry.message_attributes(name, attribute);
                    }
                }
                for (name, value) in parameters.message_system_attributes.iter().flatten() {
                    if let Some(attribute) = system_attribute(subscriber, name, value) {
                        entry = entry.message_system_attributes(
                            ::aws_sdk_sqs::types::MessageSystemAttributeNameForSends::AwsTraceHeader,
                            attribute,
                        );
                    }
                }
            }
            match entry.build() {
                Ok(entry) => request = request.entries(entry),
                Err(error) => ::tracing::warn!(
                    subscriber = %subscriber.arn,
                    %error,
                    "an EventBridge subscriber's delivery could not be built as a queue message"
                ),
            }
        }
        match request.send().await {
            Ok(answer) => {
                for failed in answer.failed() {
                    ::tracing::warn!(
                        subscriber = %subscriber.arn,
                        queue = %queue_url,
                        code = %failed.code(),
                        "an EventBridge subscriber's delivery was refused by its queue target"
                    );
                    // Which entry the queue refused, read back off the id this
                    // send gave it: an id the queue did not take from this
                    // batch names no delivery of it.
                    match failed
                        .id()
                        .parse::<usize>()
                        .ok()
                        .and_then(|position| bodies.get(position))
                    {
                        Some(delivery) => undelivered.push((
                            delivery,
                            Failure::entry(failed.code(), failed.message(), queue_arn),
                        )),
                        None => ::tracing::warn!(
                            subscriber = %subscriber.arn,
                            queue = %queue_url,
                            "a queue target reported a batch entry this delivery did not send"
                        ),
                    }
                }
            }
            Err(error) => {
                ::tracing::warn!(
                    subscriber = %subscriber.arn,
                    queue = %queue_url,
                    error = ?error,
                    "an EventBridge subscriber's delivery did not reach its queue target"
                );
                // The whole call failed, so every event of this batch failed
                // with it — unless the answer is no refusal of the event at
                // all ([`Failure::answered`]), which is a gap in this emulator
                // and not a delivery AWS would record as failed.
                if let Some(failure) = Failure::answered(&error, queue_arn) {
                    undelivered.extend(
                        chunk
                            .iter()
                            .map(|delivery| (delivery, failure.clone())),
                    );
                }
            }
        }
    }
    undelivered
}

/// One `MessageAttributes` entry of the subscriber's `SqsParameters` as the
/// queue takes it, or `None` for one this delivery cannot carry.
///
/// A text value travels with its declared `DataType`, defaulting to `String`
/// when the subscriber named none, since that is the type of the value it
/// carries. A `BinaryValue` is NOT carried: the queue takes binary as bytes and
/// the encoding of this member's text is not stated, so guessing one would put
/// an attribute on the message the subscriber did not configure.
///
/// `SqsParameters` is a @sensitive shape, so no value of it reaches a log: what
/// is reported is the subscriber and what could not be carried.
fn message_attribute(
    subscriber: &Subscriber,
    value: &crate::ops::types::SqsMessageAttributeValue,
) -> Option<::aws_sdk_sqs::types::MessageAttributeValue> {
    let Some(text) = value.string_value.as_deref() else {
        ::tracing::warn!(
            subscriber = %subscriber.arn,
            "an EventBridge subscriber's message attribute carries no StringValue and was not \
             carried to its queue target"
        );
        return None;
    };
    let data_type = value
        .data_type
        .as_deref()
        .filter(|declared| !declared.is_empty())
        .unwrap_or(STRING_ATTRIBUTE_TYPE);
    match ::aws_sdk_sqs::types::MessageAttributeValue::builder()
        .data_type(data_type)
        .string_value(text)
        .build()
    {
        Ok(attribute) => Some(attribute),
        Err(error) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                %error,
                "an EventBridge subscriber's message attribute could not be built"
            );
            None
        }
    }
}

/// One `MessageSystemAttributes` entry as the queue takes it, or `None` for a
/// name the queue accepts no system attribute under.
///
/// Amazon SQS accepts exactly one system attribute on a send,
/// `AWSTraceHeader`, so an entry named anything else is not carried: the queue
/// would refuse the whole send for it.
fn system_attribute(
    subscriber: &Subscriber,
    name: &str,
    value: &crate::ops::types::SqsMessageAttributeValue,
) -> Option<::aws_sdk_sqs::types::MessageSystemAttributeValue> {
    if name != TRACE_HEADER_ATTRIBUTE {
        ::tracing::warn!(
            subscriber = %subscriber.arn,
            "an EventBridge subscriber's message system attribute is not one Amazon SQS accepts \
             on a send and was not carried to its queue target"
        );
        return None;
    }
    let text = value.string_value.as_deref()?;
    let data_type = value
        .data_type
        .as_deref()
        .filter(|declared| !declared.is_empty())
        .unwrap_or(STRING_ATTRIBUTE_TYPE);
    match ::aws_sdk_sqs::types::MessageSystemAttributeValue::builder()
        .data_type(data_type)
        .string_value(text)
        .build()
    {
        Ok(attribute) => Some(attribute),
        Err(error) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                attribute = %name,
                %error,
                "an EventBridge subscriber's message system attribute could not be built"
            );
            None
        }
    }
}

/// The `QueueUrl` Amazon SQS addresses the queue of `name` in `account` by, or
/// `None` when the ARN's fields name no queue.
///
/// A queue's ARN names the queue DIRECTLY
/// (`arn:<partition>:sqs:<region>:<account>:<QueueName>`), and the url every SQS
/// operation takes is `<endpoint>/<account>/<QueueName>`. A resource field
/// carrying a type separator addresses no queue, and an account field that is
/// not an account id names none either — so neither reaches the url, which is
/// what keeps a caller-written ARN from reshaping the address this delivery is
/// made to.
fn queue_url(region: &str, account: &str, name: &str) -> Option<String> {
    /// How many digits an account id carries.
    const ACCOUNT_ID_DIGITS: usize = 12;

    let account_names_an_account =
        account.len() == ACCOUNT_ID_DIGITS && account.bytes().all(|byte| byte.is_ascii_digit());
    if !account_names_an_account || name.contains('/') || name.contains(':') {
        return None;
    }
    Some(format!("https://sqs.{region}.amazonaws.com/{account}/{name}"))
}

/// Build the Amazon SQS client over the IN-PROCESS callback into this same
/// emulator, against that service's endpoint in `region`.
///
/// The transport is in-process, so the client disables what a network client
/// needs and this one cannot have: retries, timeouts, stalled-stream protection
/// and identity caching each need a sleep implementation the callback transport
/// is not given, and there is nothing to retry behind it anyway.
fn sqs_client(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_sqs::Client {
    let config = ::aws_sdk_sqs::Config::builder()
        .behavior_version(::aws_sdk_sqs::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_sqs::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://sqs.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_sqs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_sqs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_sqs::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_sqs::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_sqs::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::{queue_url, Target, PUBLISH_ACTION, SEND_MESSAGE_ACTION};

    /// The Region of the bus a subscriber whose target names none belongs to.
    const BUS_REGION: &str = "eu-west-2";

    /// One row of the resolution table: the case, the ARN it configures, and —
    /// for a value that names a resource — the Region that resource is reached
    /// in together with the action the call on it performs.
    type ResolvedRow = (
        &'static str,
        &'static str,
        Option<(&'static str, &'static str)>,
    );

    /// A queue url is built only from fields that really name a queue of an
    /// account: the rows differ along the resource field and the account field.
    #[test]
    fn a_queue_url_is_built_only_from_fields_that_name_a_queue() {
        // (case, account, resource, the url the pair addresses)
        let rows: [(&str, &str, &str, Option<&str>); 6] = [
            (
                "a queue name",
                "000000000000",
                "orders",
                Some("https://sqs.eu-west-2.amazonaws.com/000000000000/orders"),
            ),
            (
                "a FIFO queue name, whose name carries a suffix",
                "000000000000",
                "orders.fifo",
                Some("https://sqs.eu-west-2.amazonaws.com/000000000000/orders.fifo"),
            ),
            ("a resource carrying a path", "000000000000", "../other", None),
            ("a resource carrying a type", "000000000000", "queue:orders", None),
            ("an account that is no account id", "my-account", "orders", None),
            ("an account of the wrong length", "0000", "orders", None),
        ];
        for (case, account, resource, expected) in rows {
            let built = queue_url("eu-west-2", account, resource);
            assert_eq!(
                built.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {built:?}"
            );
        }
    }

    /// RULE: a configured ARN addresses a resource only where it really names
    /// one of the kinds this delivery calls, it is reached in the Region its own
    /// ARN names (the subscriber's bus's Region where it names none), and the
    /// action the call performs follows from that kind.
    ///
    /// The rows differ along the service the ARN names, the form of its resource
    /// field and whether it carries a Region label at all, which is what the
    /// rule generalises over.
    #[test]
    fn a_target_is_the_resource_its_own_arn_names() {
        // (case, the configured ARN, the Region it is reached in and the action
        // the call performs, or `None` for a value naming no such resource)
        let rows: [ResolvedRow; 10] = [
            (
                "a queue",
                "arn:aws:sqs:us-west-2:000000000000:orders",
                Some(("us-west-2", SEND_MESSAGE_ACTION)),
            ),
            (
                "a topic",
                "arn:aws:sns:us-west-2:000000000000:fanout",
                Some(("us-west-2", PUBLISH_ACTION)),
            ),
            (
                "a topic of another partition",
                "arn:aws-us-gov:sns:us-gov-west-1:000000000000:fanout",
                Some(("us-gov-west-1", PUBLISH_ACTION)),
            ),
            (
                "a topic carrying no Region label, which is the bus's own",
                "arn:aws:sns::000000000000:fanout",
                Some((BUS_REGION, PUBLISH_ACTION)),
            ),
            (
                "an SNS subscription rather than the topic",
                "arn:aws:sns:us-west-2:000000000000:fanout:8f3a0e2c",
                None,
            ),
            (
                "a queue whose account is no account id",
                "arn:aws:sqs:us-west-2:my-account:orders",
                None,
            ),
            (
                "a Lambda function",
                "arn:aws:lambda:us-west-2:000000000000:function:handler",
                None,
            ),
            (
                "an event bus of this service",
                "arn:aws:events:us-west-2:000000000000:event-busv2/orders/abc",
                None,
            ),
            ("a topic naming no topic at all", "arn:aws:sns:us-west-2:000000000000:", None),
            ("not an ARN at all", "orders", None),
        ];
        for (case, arn, expected) in rows {
            let resolved = Target::resolve(arn, BUS_REGION);
            let answered = resolved
                .as_ref()
                .ok()
                .map(|target| (target.region(), target.action()));
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
            if let Ok(target) = &resolved {
                assert_eq!(
                    target.arn(),
                    arn,
                    "case {case:?}: a resource is named by the ARN that was configured"
                );
            }
        }
    }
}
