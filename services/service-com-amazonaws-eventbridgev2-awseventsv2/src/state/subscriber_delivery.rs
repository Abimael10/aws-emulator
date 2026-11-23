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

mod bus_target;
mod dead_letter;
mod delivery_authorization;
mod topic_target;

use std::collections::VecDeque;

use super::subscribers::{
    delivered_body, delivered_event, filters_match, DeliveredEvent, DueDelivery, Subscriber,
};
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

/// The IAM action the call a delivery to another EVENT BUS makes performs:
/// publishing the forwarded event onto that bus, which is this service's own
/// `PutEvents` under its `arnNamespace` ([`super::ARN_SERVICE`]).
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// How many forwards one publish may be followed through.
///
/// EMULATOR bound, not an AWS limit: a subscriber of bus A may forward onto bus
/// B whose own subscriber forwards back onto A, so the chain a single publish
/// sets off is bounded here — without it one accepted event would be published
/// forever. A chain stopped at this bound is reported to the log and forwards
/// nothing further, so no delivery is claimed for an event nothing published.
const MAX_FORWARD_HOPS: usize = 4;

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

/// The `DataType` a message attribute carrying bytes declares, the other
/// logical type Amazon SQS publishes for the member ("Binary").
const BINARY_ATTRIBUTE_TYPE: &str = "Binary";

/// The suffix the NAME of a FIFO queue or topic carries, per the
/// `DeadLetterQueueArn` pattern this service's own model writes a queue ARN with
/// (`([A-Za-z0-9_-]{1,80}|[A-Za-z0-9_-]{1,75}\.fifo)`).
pub(super) const FIFO_SUFFIX: &str = ".fifo";

/// Whether the resource `arn` names is a FIFO one, which takes no message
/// without a group and no per-message delay.
pub(super) fn is_fifo_target(arn: &str) -> bool {
    ::service_common::arn::parse(arn)
        .is_some_and(|parsed| parsed.resource.ends_with(FIFO_SUFFIX))
}

/// What the configured value `value` carries for this delivery: its expression's
/// text when it is one, and the value itself when it is a literal
/// ([`super::event_expressions::configured_text`]).
///
/// `None` is a member the subscriber configured nothing for, and one whose
/// expression produced nothing or could not be evaluated: the value does not
/// travel, rather than travelling as the text of the expression that was meant
/// to produce it.
fn configured(
    subscriber: &Subscriber,
    delivery: &Delivery,
    value: Option<&str>,
    budget: &mut super::event_expressions::EvaluationBudget,
) -> Option<String> {
    let configured = value?;
    match super::event_expressions::configured_text(
        configured,
        &delivery.event,
        super::event_expressions::MAX_SCALAR_BYTES,
        budget,
    ) {
        Ok(resolved) => resolved,
        Err(why) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                event = %delivery.event_id(),
                "an EventBridge subscriber's configured value was not carried to its target: \
                 {why}"
            );
            None
        }
    }
}

/// The `MessageGroupId` this delivery travels with: `configured_group`, the one
/// the subscriber's target parameters name ("message group ID for FIFO queues.
/// Accepts JSONata expression"), and for a FIFO target the subscriber names none
/// for, the event's own `EventGroupId` — "event group ID for FIFO ordering",
/// which is the order the publisher asked its events to keep.
///
/// A FIFO target "fails the action" for a message carrying no group at all, so a
/// delivery to one always carries a group; a standard one carries a group only
/// where the subscriber configured it, since it orders nothing there.
fn group_of(
    subscriber: &Subscriber,
    delivery: &Delivery,
    configured_group: Option<&str>,
    fifo: bool,
    budget: &mut super::event_expressions::EvaluationBudget,
) -> Option<String> {
    if let Some(group) = configured(subscriber, delivery, configured_group, budget) {
        return Some(group);
    }
    if !fifo {
        return None;
    }
    delivery
        .event
        .event_group_id()
        .map(str::to_string)
        // A FIFO target takes no message without a group, and an event
        // published without one belongs to no group of its own, so it travels
        // under the subscriber that is delivering it: its own events stay in
        // publication order and no other subscriber's are ordered against them.
        .or_else(|| Some(subscriber.arn.clone()))
}

/// The `MessageDeduplicationId` this delivery travels with: `configured_token`,
/// the one the subscriber's target parameters name, and for a FIFO target the
/// subscriber names none for, the event's own identity.
///
/// The member is "message deduplication ID for FIFO queues", and a STANDARD
/// target takes no deduplication token at all — Amazon SQS refuses the whole
/// send for "a parameter that applies only to FIFO queues" — so one travels only
/// to a FIFO target, whatever the subscriber configured.
///
/// A FIFO target refuses a message carrying no deduplication token unless it
/// deduplicates by content itself ("if you don't provide a
/// MessageDeduplicationId and the queue doesn't have ContentBasedDeduplication
/// set, the action fails"), so a delivery to one carries the `EventId` of the
/// event it delivers: each accepted event is one event, so no delivery is
/// suppressed as a duplicate of another — the duplicates the bus itself
/// identified never became events at all.
fn deduplication_token_of(
    subscriber: &Subscriber,
    delivery: &Delivery,
    configured_token: Option<&str>,
    fifo: bool,
    budget: &mut super::event_expressions::EvaluationBudget,
) -> Option<String> {
    if !fifo {
        return None;
    }
    configured(subscriber, delivery, configured_token, budget)
        .or_else(|| Some(delivery.event_id().to_string()))
}

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
/// charged to ONE budget across every subscriber and event, and so is the
/// evaluation of the expressions the subscribers' configurations carry
/// ([`super::event_expressions::EvaluationBudget`]), so a request can never buy
/// more matching or evaluation work than those budgets sell.
///
/// A subscriber whose target is another EVENT BUS publishes what it was owed
/// onto that bus ([`bus_target`]), which makes the subscribers of that bus owed
/// it in turn: those deliveries are made here too, so one accepted event
/// reaches the end of a fan-out chain within the publish that accepted it. The
/// chain is walked rather than recursed, and what it may walk is bounded BOTH
/// ways: [`MAX_FORWARD_HOPS`] bounds how far from the publish a forward may be
/// made, and [`MAX_DELIVERY_WINDOWS_PER_CALL`] bounds how many windows the whole
/// walk may make — which is what bounds the PRODUCT a chain of forwarding
/// subscribers would otherwise multiply out.
///
/// What each window COMPUTES — the filters, the body each delivery carries and
/// the values the target's own API takes — is resolved on a blocking thread
/// ([`prepare`]) rather than on the async worker serving the request, because a
/// caller's expression is evaluated by an engine and that work blocks the thread
/// it runs on.
pub async fn deliver_due(
    store: &::engine::ServiceState<super::ComAmazonawsEventbridgev2Awseventsv2State>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    due: &[DueDelivery],
) {
    let mut budget = ::service_common::event_pattern::MatchBudget::new(
        ::service_common::event_pattern::MAX_MATCH_STEPS_PER_REQUEST,
    );
    let mut evaluations = super::event_expressions::EvaluationBudget::new();
    let mut windows = MAX_DELIVERY_WINDOWS_PER_CALL;
    // Each entry is one subscriber's due window beside the number of forwards
    // that made it due, which is what [`MAX_FORWARD_HOPS`] bounds.
    let mut owed: VecDeque<(usize, DueDelivery)> =
        due.iter().map(|owed| (0, owed.clone())).collect();
    while let Some((hop, owed_now)) = owed.pop_front() {
        let Some(left) = windows.checked_sub(1) else {
            ::tracing::warn!(
                pending = owed.len() + 1,
                "an EventBridge publish made more than the {MAX_DELIVERY_WINDOWS_PER_CALL} \
                 delivery windows this emulator follows for one call, so the rest were not \
                 delivered"
            );
            return;
        };
        windows = left;
        let subscriber = owed_now.subscriber;
        let target = match Target::resolve(subscriber.target_arn(), &subscriber.region) {
            Ok(target) => target,
            Err(reason) => {
                ::tracing::warn!(
                    subscriber = %subscriber.arn,
                    target = %subscriber.target_arn(),
                    "an EventBridge subscriber's delivery was not attempted: {reason}"
                );
                continue;
            }
        };
        let prepared;
        (prepared, budget, evaluations) = prepare(
            subscriber.clone(),
            owed_now.events,
            target.kind(),
            budget,
            evaluations,
        )
        .await;
        if prepared.is_empty() {
            continue;
        }
        for forwarded in
            deliver_prepared(store, callback, &subscriber, &target, &prepared, hop).await
        {
            owed.push_back((hop + 1, forwarded));
        }
    }
}

/// How many delivery windows one call may follow.
///
/// EMULATOR bound: a forward makes the subscribers of the downstream bus owed
/// the event, and each of those may forward again, so the windows one publish
/// sets off are the product of how many subscribers each bus on the chain holds
/// and how far the chain runs — a product neither factor bounds. This bounds it
/// for the whole call; a walk that reaches it stops and says so, so no delivery
/// is reported for a window nobody made.
const MAX_DELIVERY_WINDOWS_PER_CALL: usize = 64;

/// Resolve everything one subscriber's due window COMPUTES, on a blocking
/// thread: the filters each event is held to, the body its `Transformer`
/// produces and the values the target's own API takes for it.
///
/// A caller's expression is evaluated by an ENGINE — CPU-bound work that blocks
/// the thread it runs on until it finishes or its clock stops it — so it must
/// not run on the async worker serving the request: `spawn_blocking` moves the
/// whole resolution to the blocking pool, which leaves this call a real await
/// point and keeps the runtime answering health checks, timeouts and shutdown
/// while a publish resolves its deliveries.
///
/// The budgets travel in and out BY VALUE, because the work happens on another
/// thread: what the window spent is handed back for the next one. A resolution
/// that did not finish at all hands back SPENT budgets, so a call whose
/// resolution failed does no further resolving.
async fn prepare(
    subscriber: Subscriber,
    events: Vec<super::StoredEvent>,
    kind: Kind,
    budget: ::service_common::event_pattern::MatchBudget,
    evaluations: super::event_expressions::EvaluationBudget,
) -> (
    Prepared,
    ::service_common::event_pattern::MatchBudget,
    super::event_expressions::EvaluationBudget,
) {
    let arn = subscriber.arn.clone();
    match ::tokio::task::spawn_blocking(move || {
        resolve(&subscriber, &events, kind, budget, evaluations)
    })
    .await
    {
        Ok(resolved) => resolved,
        Err(error) => {
            ::tracing::warn!(
                subscriber = %arn,
                %error,
                "an EventBridge subscriber's deliveries were not resolved, so none was made"
            );
            (
                Prepared::nothing(kind),
                ::service_common::event_pattern::MatchBudget::new(0),
                super::event_expressions::EvaluationBudget::spent(),
            )
        }
    }
}

/// The resolution itself, as it runs on the blocking thread ([`prepare`]).
fn resolve(
    subscriber: &Subscriber,
    events: &[super::StoredEvent],
    kind: Kind,
    mut budget: ::service_common::event_pattern::MatchBudget,
    mut evaluations: super::event_expressions::EvaluationBudget,
) -> (
    Prepared,
    ::service_common::event_pattern::MatchBudget,
    super::event_expressions::EvaluationBudget,
) {
    let mut prepared = Prepared::nothing(kind);
    for stored in events {
        let event = delivered_event(subscriber, stored);
        if !filters_match(subscriber.filter_configuration.as_ref(), &event, &mut budget) {
            continue;
        }
        match delivered_body(subscriber, &event, &mut evaluations) {
            Ok(body) => prepared.prepare(subscriber, Delivery { event, body }, &mut evaluations),
            Err(why) => ::tracing::warn!(
                subscriber = %subscriber.arn,
                event = %event.event_id,
                "an EventBridge subscriber was not delivered an event: {why}"
            ),
        }
    }
    (prepared, budget, evaluations)
}

/// The KIND of resource a subscriber invokes, as the resolution of its
/// deliveries needs to know it: which call they travel in.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Queue,
    Topic,
    EventBus,
}

/// One subscriber's due window with every expression of its configuration
/// already resolved: each delivery beside the call it travels in, built for the
/// kind of target the subscriber invokes.
///
/// The two are held TOGETHER, so what the target is handed and what a record of
/// a delivery that did not happen names cannot come apart: a delivery whose call
/// could not be built is in neither.
#[derive(Debug)]
enum Prepared {
    Queue(Vec<(Delivery, ::aws_sdk_sqs::types::SendMessageBatchRequestEntry)>),
    Topic(Vec<(Delivery, ::aws_sdk_sns::types::PublishBatchRequestEntry)>),
    EventBus(Vec<(Delivery, crate::state::ForwardedEvent)>),
}

impl Prepared {
    /// Nothing prepared yet, for a target of this kind.
    fn nothing(kind: Kind) -> Self {
        match kind {
            Kind::Queue => Self::Queue(Vec::new()),
            Kind::Topic => Self::Topic(Vec::new()),
            Kind::EventBus => Self::EventBus(Vec::new()),
        }
    }

    /// Whether this window prepared no delivery at all, in which case no call
    /// is made for it.
    fn is_empty(&self) -> bool {
        match self {
            Self::Queue(prepared) => prepared.is_empty(),
            Self::Topic(prepared) => prepared.is_empty(),
            Self::EventBus(prepared) => prepared.is_empty(),
        }
    }

    /// The deliveries of this window, in publication order.
    fn deliveries(&self) -> impl Iterator<Item = &Delivery> {
        let prepared: Box<dyn Iterator<Item = &Delivery>> = match self {
            Self::Queue(prepared) => Box::new(prepared.iter().map(|(delivery, _)| delivery)),
            Self::Topic(prepared) => Box::new(prepared.iter().map(|(delivery, _)| delivery)),
            Self::EventBus(prepared) => Box::new(prepared.iter().map(|(delivery, _)| delivery)),
        };
        prepared
    }

    /// Resolve the call `delivery` travels in and hold the two together, or drop
    /// the delivery where its call could not be built — which is reported and
    /// never reported as delivered.
    fn prepare(
        &mut self,
        subscriber: &Subscriber,
        delivery: Delivery,
        budget: &mut super::event_expressions::EvaluationBudget,
    ) {
        match self {
            Self::Queue(prepared) => {
                let position = prepared.len();
                if let Some(entry) = queue_entry(subscriber, &delivery, position, budget) {
                    prepared.push((delivery, entry));
                }
            }
            Self::Topic(prepared) => {
                let position = prepared.len();
                if let Some(entry) = topic_target::entry(subscriber, &delivery, position, budget) {
                    prepared.push((delivery, entry));
                }
            }
            Self::EventBus(prepared) => {
                let forwarded = bus_target::forwarded(subscriber, &delivery, budget);
                prepared.push((delivery, forwarded));
            }
        }
    }
}

/// One accepted event as the delivery hands it over: the text the target is
/// given, beside the event it carries — whose identity is what a log or a
/// record of a delivery that did not happen names, and whose own parts are what
/// an expression of the subscriber's configuration reads
/// ([`super::event_expressions`]).
#[derive(Debug, Clone)]
struct Delivery {
    /// The event this delivery carries, as its subscriber sees it.
    event: DeliveredEvent,
    /// The event in the shape the subscriber's `Transformer` names.
    body: String,
}

impl Delivery {
    /// The `EventId` the publish reported for the event this delivery carries.
    fn event_id(&self) -> &str {
        &self.event.event_id
    }
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
    /// Another event bus of this service: each event is published onto it as a
    /// new event of that bus ([`bus_target`]).
    EventBus {
        /// The bus's `EventBusArn` as it was configured, which is both the
        /// resource the publish is authorized on and the bus it appends to.
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
const NAMES_NO_EVENT_BUS: &str = "it names no event bus";
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
            // An ARN of THIS service's own namespace names an event bus when
            // its resource field is one (`event-busv2/<name>/<id>`), which is
            // the only resource of this service an event can be delivered to;
            // every other resource of it (a subscriber, an event source) takes
            // no event.
            super::ARN_SERVICE => {
                let names_a_bus = parsed.resource_type() == super::ARN_RESOURCE_TYPE;
                if names_a_bus {
                    Ok(Self::EventBus {
                        arn: arn.to_string(),
                        region,
                    })
                } else {
                    Err(NAMES_NO_EVENT_BUS)
                }
            }
            _ => Err(NAMES_AN_UNCALLED_SERVICE),
        }
    }

    /// The ARN this resource was configured by: what a record of a failed
    /// delivery names, and the resource the write on it is authorized on.
    fn arn(&self) -> &str {
        match self {
            Self::Queue { arn, .. } | Self::Topic { arn, .. } | Self::EventBus { arn, .. } => arn,
        }
    }

    /// The Region this resource is reached in.
    fn region(&self) -> &str {
        match self {
            Self::Queue { region, .. }
            | Self::Topic { region, .. }
            | Self::EventBus { region, .. } => region,
        }
    }

    /// The IAM action the call that hands this resource an event performs.
    fn action(&self) -> &'static str {
        match self {
            Self::Queue { .. } => SEND_MESSAGE_ACTION,
            Self::Topic { .. } => PUBLISH_ACTION,
            Self::EventBus { .. } => PUT_EVENTS_ACTION,
        }
    }

    /// The KIND of resource this is, as the resolution of a window's deliveries
    /// needs it: which call those deliveries travel in.
    fn kind(&self) -> Kind {
        match self {
            Self::Queue { .. } => Kind::Queue,
            Self::Topic { .. } => Kind::Topic,
            Self::EventBus { .. } => Kind::EventBus,
        }
    }
}

/// Hand this window's prepared deliveries to the resource the subscriber's
/// `InvokeConfiguration` names, through the one API that resource's service is
/// invoked by, AS THE ROLE the subscriber invokes its target with — and record
/// what did not get there.
///
/// Every value these calls carry was resolved before this one was made
/// ([`prepare`]), so nothing is evaluated here: what happens on the async
/// worker is the calls themselves.
///
/// The deliveries this one made due are reported back: a forward onto another
/// event bus of this service makes the subscribers of THAT bus owed the
/// forwarded event, and those deliveries are made by the caller
/// ([`deliver_due`]) rather than from inside this call.
async fn deliver_prepared<'a>(
    store: &::engine::ServiceState<super::ComAmazonawsEventbridgev2Awseventsv2State>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    subscriber: &Subscriber,
    target: &Target,
    prepared: &'a Prepared,
    hop: usize,
) -> Vec<DueDelivery> {
    // The role decides the call BEFORE it is made: a role the permissions in
    // force on it do not let make that call is a delivery that does not happen,
    // so the target is handed nothing and every event of this window is
    // recorded as failed instead. WHICH document withheld it is reported to the
    // log, because the record a client reads states the condition rather than
    // the document.
    let mut made_due = Vec::new();
    let undelivered: Vec<(&'a Delivery, Failure)> = match delivery_authorization::withheld(
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
            prepared
                .deliveries()
                .map(|delivery| (delivery, Failure::inaccessible(target.arn())))
                .collect()
        }
        None => match (prepared, target) {
            (Prepared::Queue(entries), Target::Queue { url, .. }) => {
                send_to_queue(callback, subscriber, target.region(), url, target.arn(), entries)
                    .await
            }
            (Prepared::Topic(entries), Target::Topic { arn, .. }) => {
                topic_target::publish(callback, subscriber, target.region(), arn, entries).await
            }
            (Prepared::EventBus(entries), Target::EventBus { arn, .. }) => {
                let forwarded = bus_target::forward(store, subscriber, arn, entries, hop);
                made_due = forwarded.made_due;
                forwarded.undelivered
            }
            // The window was prepared FOR this target ([`Target::kind`]), so
            // the two always agree; a pair that did not would be a window
            // nothing was prepared for, which is no delivery and no failure.
            _ => Vec::new(),
        },
    };
    dead_letter::park(callback, subscriber, target.arn(), &undelivered).await;
    made_due
}

/// The `SendMessageBatch` entry one delivery to a queue travels as, or `None`
/// for a delivery the queue could take no entry for — which is reported and
/// never reported as delivered.
///
/// The values the subscriber's `SqsParameters` carries travel with the send:
/// "Values are forwarded to the SQS `SendMessageBatch` API" (`SqsParameters`),
/// which is the API this delivery makes. Each of those values is "a literal or a
/// JSONata expression", so it is resolved against the event the entry carries
/// ([`configured`]) — once per delivered event, which is what
/// `SqsMessageAttributeValue$StringValue` states.
///
/// A FIFO queue takes no message without a group and, unless it deduplicates by
/// content itself, none without a deduplication token, so a delivery to one
/// carries both ([`group_of`], [`deduplication_token_of`]) — and carries no
/// per-message delay, which such a queue refuses and which this model documents
/// for "standard queues only".
fn queue_entry(
    subscriber: &Subscriber,
    delivery: &Delivery,
    position: usize,
    budget: &mut super::event_expressions::EvaluationBudget,
) -> Option<::aws_sdk_sqs::types::SendMessageBatchRequestEntry> {
    let parameters = subscriber.invoke_configuration.sqs_parameters.as_ref();
    let fifo = is_fifo_target(subscriber.target_arn());
    let mut entry = ::aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
        // An entry id identifies one message WITHIN its batch ("an identifier
        // for a message in this batch used to communicate the result"), and the
        // character set it is held to is alphanumerics, hyphens and
        // underscores. It is this delivery's position in the window, so what the
        // queue reports about an entry is read back onto the event it carried.
        .id(position.to_string())
        .message_body(delivery.body.as_str());
    if let Some(group) = group_of(
        subscriber,
        delivery,
        parameters.and_then(|parameters| parameters.message_group_id.as_deref()),
        fifo,
        budget,
    ) {
        entry = entry.message_group_id(group);
    }
    if let Some(token) = deduplication_token_of(
        subscriber,
        delivery,
        parameters.and_then(|parameters| parameters.message_deduplication_id.as_deref()),
        fifo,
        budget,
    ) {
        entry = entry.message_deduplication_id(token);
    }
    if let Some(parameters) = parameters {
        // `DelaySeconds` is text in this model, and the queue takes a number: a
        // value that is not one carries no delay rather than one nobody asked
        // for. `SqsParameters` is a @sensitive shape, so what a refusal reports
        // is the member and not its value.
        let delay = (!fifo)
            .then(|| {
                configured(
                    subscriber,
                    delivery,
                    parameters.delay_seconds.as_deref(),
                    budget,
                )
            })
            .flatten();
        if let Some(delay) = delay {
            match delay.parse::<i32>() {
                Ok(seconds) => entry = entry.delay_seconds(seconds),
                Err(error) => ::tracing::warn!(
                    subscriber = %subscriber.arn,
                    %error,
                    "an EventBridge subscriber's DelaySeconds is not a number of seconds and was \
                     not carried to its queue target"
                ),
            }
        }
        for (name, value) in parameters.message_attributes.iter().flatten() {
            // The NAME of an attribute is a configured value too, so it is
            // resolved against the event exactly as its value is: an attribute
            // map's "keys and values are unconstrained strings so either side
            // can contain a JSONata expression", which is what the metadata map
            // of a forwarding subscriber states for the same kind of member.
            let Some(name) = configured(subscriber, delivery, Some(name.as_str()), budget) else {
                continue;
            };
            if let Some(attribute) = message_attribute(subscriber, delivery, value, budget) {
                entry = entry.message_attributes(name, attribute);
            }
        }
        for (name, value) in parameters.message_system_attributes.iter().flatten() {
            let Some(name) = configured(subscriber, delivery, Some(name.as_str()), budget) else {
                continue;
            };
            if let Some(attribute) = system_attribute(subscriber, delivery, &name, value, budget) {
                entry = entry.message_system_attributes(
                    ::aws_sdk_sqs::types::MessageSystemAttributeNameForSends::AwsTraceHeader,
                    attribute,
                );
            }
        }
    }
    match entry.build() {
        Ok(entry) => Some(entry),
        Err(error) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                %error,
                "an EventBridge subscriber's delivery could not be built as a queue message"
            );
            None
        }
    }
}

/// Send the prepared entries to the queue at `queue_url`, in batches the queue
/// accepts, and report the deliveries it did not take.
async fn send_to_queue<'a>(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    subscriber: &Subscriber,
    region: &str,
    queue_url: &str,
    queue_arn: &str,
    prepared: &'a [(Delivery, ::aws_sdk_sqs::types::SendMessageBatchRequestEntry)],
) -> Vec<(&'a Delivery, Failure)> {
    let client = sqs_client(callback, region);
    let mut undelivered = Vec::new();
    for chunk in prepared.chunks(MAX_QUEUE_BATCH_ENTRIES) {
        let mut request = client.send_message_batch().queue_url(queue_url);
        for (_, entry) in chunk {
            request = request.entries(entry.clone());
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
                        .and_then(|position| prepared.get(position))
                    {
                        Some((delivery, _)) => undelivered.push((
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
                            .map(|(delivery, _)| (delivery, failure.clone())),
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
/// The attribute travels in the field the type it declares names, carrying the
/// value the subscriber configured read as that type's own encoding: a text
/// value is "a JSONata expression [that] resolves once per delivered event" or
/// the literal it is written as ([`configured`]), and a BINARY attribute's value
/// is the "base64-encoded literal binary attribute value" its member documents —
/// a LITERAL, so no expression is read out of it, and the bytes it encodes are
/// what the queue is handed, since Amazon SQS carries a `Binary` attribute's
/// value as bytes and refuses one supplied as text.
///
/// An attribute this delivery cannot carry is DROPPED rather than sent: Amazon
/// SQS refuses an attribute whose declared type and value disagree, or whose
/// value is empty, and the entry it is on with it — so an attribute the
/// subscriber configured unusably must not cost the event its delivery.
///
/// `SqsParameters` is a @sensitive shape, so no value of it reaches a log: what
/// is reported is the subscriber and what could not be carried.
fn message_attribute(
    subscriber: &Subscriber,
    delivery: &Delivery,
    value: &crate::ops::types::SqsMessageAttributeValue,
    budget: &mut super::event_expressions::EvaluationBudget,
) -> Option<::aws_sdk_sqs::types::MessageAttributeValue> {
    let mut attribute = ::aws_sdk_sqs::types::MessageAttributeValue::builder();
    match carried_value(subscriber, delivery, value, budget) {
        Some(CarriedValue::Binary { data_type, bytes }) => {
            attribute = attribute
                .data_type(data_type)
                .binary_value(::aws_smithy_types::Blob::new(bytes));
        }
        Some(CarriedValue::Text { data_type, text }) => {
            attribute = attribute.data_type(data_type).string_value(text);
        }
        None => return None,
    }
    match attribute.build() {
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

/// What one configured attribute is carried as: the field its value travels in,
/// the `DataType` that field admits, and the value itself.
enum CarriedValue {
    /// Bytes, under a `Binary` type.
    Binary {
        data_type: String,
        bytes: Vec<u8>,
    },
    /// Text, under a `String` or `Number` type.
    Text { data_type: String, text: String },
}

/// The value `value` is carried as, or `None` for one this delivery cannot
/// carry at all ([`message_attribute`]).
///
/// The DECLARED type decides the field: an attribute declaring a binary type
/// carries bytes, which its `BinaryValue` holds base64-encoded and which its
/// `StringValue` holds the same way where that is the member the subscriber
/// filled, and every other declared type carries text. An attribute declaring no
/// type at all — the member's own default is empty, which Amazon SQS takes no
/// attribute with — is carried as the type of the value it configured.
fn carried_value(
    subscriber: &Subscriber,
    delivery: &Delivery,
    value: &crate::ops::types::SqsMessageAttributeValue,
    budget: &mut super::event_expressions::EvaluationBudget,
) -> Option<CarriedValue> {
    let declared = value
        .data_type
        .as_deref()
        .filter(|declared| !declared.is_empty());
    let binary = match declared {
        Some(declared) => declared.split('.').next() == Some(BINARY_ATTRIBUTE_TYPE),
        None => value.binary_value.is_some(),
    };
    if binary {
        let encoded = value
            .binary_value
            .as_deref()
            .or(value.string_value.as_deref())?;
        let Ok(bytes) = ::aws_smithy_types::base64::decode(encoded) else {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                "an EventBridge subscriber's binary message attribute carries a value that is \
                 not base64 and was not carried to its queue target"
            );
            return None;
        };
        if bytes.is_empty() {
            return None;
        }
        return Some(CarriedValue::Binary {
            data_type: declared.unwrap_or(BINARY_ATTRIBUTE_TYPE).to_string(),
            bytes,
        });
    }
    let text = configured(subscriber, delivery, value.string_value.as_deref(), budget)?;
    if text.is_empty() {
        ::tracing::warn!(
            subscriber = %subscriber.arn,
            "an EventBridge subscriber's message attribute carries no value and was not carried \
             to its queue target"
        );
        return None;
    }
    Some(CarriedValue::Text {
        data_type: declared.unwrap_or(STRING_ATTRIBUTE_TYPE).to_string(),
        text,
    })
}

/// One `MessageSystemAttributes` entry as the queue takes it, or `None` for one
/// this delivery cannot carry.
///
/// Amazon SQS accepts exactly one system attribute on a send,
/// `AWSTraceHeader`, and "its type must be String and its value must be a
/// correctly formatted X-Ray trace header string" — so an entry named anything
/// else, and one carrying no text, is not carried: the queue would refuse the
/// whole send for either. The type is the one that attribute takes rather than
/// the one the subscriber declared, for the same reason.
fn system_attribute(
    subscriber: &Subscriber,
    delivery: &Delivery,
    name: &str,
    value: &crate::ops::types::SqsMessageAttributeValue,
    budget: &mut super::event_expressions::EvaluationBudget,
) -> Option<::aws_sdk_sqs::types::MessageSystemAttributeValue> {
    if name != TRACE_HEADER_ATTRIBUTE {
        ::tracing::warn!(
            subscriber = %subscriber.arn,
            "an EventBridge subscriber's message system attribute is not one Amazon SQS accepts \
             on a send and was not carried to its queue target"
        );
        return None;
    }
    let text = configured(subscriber, delivery, value.string_value.as_deref(), budget)
        .filter(|text| !text.is_empty())?;
    match ::aws_sdk_sqs::types::MessageSystemAttributeValue::builder()
        .data_type(STRING_ATTRIBUTE_TYPE)
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
    use super::{queue_url, Target, PUBLISH_ACTION, PUT_EVENTS_ACTION, SEND_MESSAGE_ACTION};

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
        let rows: [ResolvedRow; 11] = [
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
                "an event bus of this service, which an event is forwarded onto",
                "arn:aws:events:us-west-2:000000000000:event-busv2/orders/abc",
                Some(("us-west-2", PUT_EVENTS_ACTION)),
            ),
            (
                "a subscriber of this service, which takes no event",
                "arn:aws:events:us-west-2:000000000000:subscriber/orders/abc",
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
