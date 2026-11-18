// SPDX-License-Identifier: CC0-1.0

//! The AWS IoT RULES ENGINE: what a message published to a topic DOES to the topic rules
//! this account holds.
//!
//! A rule's `sql` selects the messages it is about and its `actions` are what AWS IoT then
//! does with each of them, so a message published to a topic a stored rule's `FROM` clause
//! covers is evaluated against that rule and each of its actions is performed. Driven from
//! the data plane's `Publish` ([`dispatch_published_message`]); none of it is reported to the
//! publisher, so what a client observes is the effect on the action's TARGET — the record a
//! `kinesis` action wrote onto its stream, the event a `lambda` action handed its function.
//!
//! Three gates, in order: the rule's topic filter covers the topic
//! ([`super::topic_filter_matches`], the reading `ListTopicRules` also uses) and the rule is
//! not disabled; the statement SELECTS the message ([`super::rule_sql`], which reads the
//! statement, and [`super::rule_expression`], which evaluates it over the message and the
//! topic); and the action is one this service may make — for an action naming a role, that
//! role must let this service make the action's API call ([`super::rule_action_role`]), and
//! for a `lambda` action, which names none, the target function's own resource policy must
//! admit the invocation this service makes for THIS rule ([`lambda_action`]). An action whose
//! permission is withheld is an action NOT performed, which is the only observable the
//! permission boundary has.
//!
//! An action that FAILS is what `TopicRulePayload$errorAction` ("the action to take when an
//! error occurs") is for: every action of the rule is attempted, and if any of them was
//! refused the rule's error action is performed instead with the rule-failure report
//! ([`failure_report`]) as its message — the rule's name, the topic, the published payload and
//! the failures, so a client using `errorAction` as its dead-letter path for rule failures
//! gets the failure it was promised rather than silence.
//!
//! What this engine cannot compute faithfully it does NOT perform: a statement it cannot
//! read, a condition it cannot decide, and an action parameter carrying a substitution
//! template are each left unperformed rather than performed on a guessed value. None of those
//! is a FAILURE of the action either, because they are this emulator's limits and not the
//! service's answer, so none of them manufactures an error report.
//!
//! The actions performed today are `kinesis`, "write data to an Amazon Kinesis stream", over
//! the emulator's own in-process transport
//! ([`::service_common::kinesis_client`] — no socket, and no part of the action is a
//! destination), `sqs`, "publish data to an Amazon SQS queue", over that same transport
//! ([`::service_common::sqs_client`]), `lambda`, "invoke a Lambda function", over that same
//! transport ([`lambda_action`]), `republish`, "publish to another MQTT topic", which
//! re-injects the selected message into this same broker so the rules whose filters cover the
//! action's topic match it in turn ([`republish`]), and `http`, "send data to an HTTPS
//! endpoint", whose endpoint is a TOPIC RULE
//! DESTINATION: the message is POSTed to the action's `url` only while a destination that
//! admits that URL is `ENABLED` ([`super::topic_rule_destinations::http_action_is_allowed`]),
//! and nothing is sent otherwise — "the destination must be confirmed prior to use"
//! (`CreateTopicRuleDestination`), which is the gate the whole confirm/enable lifecycle
//! exists for. Every other member of `Action` is a target service with its own semantics (a
//! DynamoDB column per message attribute, a rendered S3 object key) and
//! its own open issue; an action this engine does not perform is left alone rather than
//! approximated.
//!
//! Every call an action makes goes over the emulator's own IN-PROCESS transport — the
//! Kinesis client's, the Amazon SQS client's, the Lambda client's and the `http` action's
//! alike — so a stream name,
//! a queue URL, a function ARN or an endpoint URL a caller configured reaches only what this
//! emulator serves itself: no socket is opened and no name is resolved.

use ::std::collections::VecDeque;

use super::rule_action_role::{role_bars_action, IamState};
use super::rule_sql::{selected_message, Selected};
use super::topic_rule_destinations::{http_action_is_allowed, http_action_unperformable};
use crate::ops::types::{
    Action, HttpAction, KinesisAction, LambdaAction, RepublishAction, SqsAction,
};

/// `Action$lambda`: the invocation a rule's `lambda` action makes, and the function
/// resource-policy grant that admits it.
pub mod lambda_action;

/// The service principal the rules engine acts as: the one an action's role must trust, and
/// the one a target function's resource policy must admit.
pub const SERVICE_PRINCIPAL: &str = "iot.amazonaws.com";

/// The IAM action a `kinesis` rule action performs on the stream it names.
pub const KINESIS_PUT_RECORD_ACTION: &str = "kinesis:PutRecord";

/// The IAM action an `sqs` rule action performs on the queue it names.
pub const SQS_SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The IAM action a `republish` rule action performs on the topic it names: republishing IS a
/// publish to the message broker, which is the call that action's role must grant.
pub const IOT_PUBLISH_ACTION: &str = "iot:Publish";

/// Amazon Kinesis's ARN namespace and the resource type of a data stream:
/// `arn:aws:kinesis:<region>:<account>:stream/<streamName>`, the resource an action's role is
/// granted [`KINESIS_PUT_RECORD_ACTION`] on.
const KINESIS_ARN_NAMESPACE: &str = "kinesis";
const STREAM_RESOURCE_TYPE: &str = "stream";

/// AWS IoT's own ARN namespace and the resource type of a message-broker topic:
/// `arn:aws:iot:<region>:<account>:topic/<topic>`, the resource a `republish` action's role is
/// granted [`IOT_PUBLISH_ACTION`] on.
const IOT_ARN_NAMESPACE: &str = "iot";
const TOPIC_RESOURCE_TYPE: &str = "topic";

/// How many actions ONE published message performs — the EMULATOR's own ceiling on the
/// fan-out of a single request, which is otherwise the (rules x actions per rule) PRODUCT of
/// a store the caller grows ([`super::MAX_TOPIC_RULES`] rules of at most ten actions each).
/// Far above any rule set a client under test writes; past it the remaining actions of the
/// message are not performed and the engine says so in its log.
const MAX_ACTIONS_PER_MESSAGE: usize = 100;

/// How many REPUBLISH hops one published message may take — the EMULATOR's own ceiling on a
/// chain of rules that republish to each other, which is otherwise unbounded (two rules that
/// republish to each other's topic would carry one message for ever). Far above the staging
/// chains a client under test writes; past it the message is not republished again and the
/// engine says so in its log.
const MAX_REPUBLISH_HOPS: usize = 4;

/// The opening of a substitution template in an action parameter. AWS IoT renders `${…}` from
/// the message; this engine does not compute those expressions, so a parameter carrying one is
/// an action it cannot perform faithfully rather than one to perform on the literal text.
const SUBSTITUTION_TEMPLATE_OPENING: &str = "${";

/// One message IN THE BROKER: the topic it was published to, its payload, and how many
/// `republish` hops carried it here.
#[derive(Debug, Clone)]
struct Published {
    /// The topic the message is on, which is what a rule's filter is matched against and what
    /// the statement's `topic()` reports.
    topic: String,
    /// The message's own bytes, as published.
    payload: Vec<u8>,
    /// How many `republish` actions this message has already passed through, bounded by
    /// [`MAX_REPUBLISH_HOPS`].
    hops: usize,
}

/// One rule a message is about, as the pass under the store's guard read it.
#[derive(Debug, Clone)]
struct Selecting {
    /// The name the rule was created under, which the rule-failure report names it by.
    rule_name: String,
    /// The rule's own ARN, which is the `aws:SourceArn` a grant for its actions is narrowed
    /// by.
    rule_arn: String,
    /// The statement that decides what the rule selects out of the message.
    sql: String,
    /// The actions the rule performs for a message it selects.
    actions: Vec<Action>,
    /// The action the rule performs instead when one of those was refused.
    error_action: Option<Action>,
}

/// Why ONE action of a rule was not carried out, as the rule-failure report states it.
#[derive(Debug, Clone)]
struct Failure {
    /// The shape of the action that failed, as the model names it (`LambdaAction`).
    failed_action: &'static str,
    /// The target it failed on: the function, stream, queue, topic or endpoint it names.
    failed_resource: String,
    /// Why it was refused, worded by whatever withheld it — the target service's own message,
    /// or the authorization sentence that says no policy allows the call.
    error_message: String,
}

/// Evaluate the message `payload` published to `topic` against every topic rule of this
/// account, and perform the actions of each rule that selects it.
///
/// Called from the data plane's `Publish` with the SAME shared engine that handler holds, so
/// the rules are the ones `CreateTopicRule` wrote and the enablement the one
/// `EnableTopicRule` / `DisableTopicRule` last left. An engine with no IoT store evaluates
/// nothing.
///
/// A `republish` action puts its message back into this same walk, so a chain of rules is
/// followed to its end rather than stopping at the first rule — which is what makes the
/// standard staging pattern (one rule normalizes onto a topic a second rule is written
/// against) work.
///
/// The work one call does is bounded: the rules are the ones the store holds (at most
/// [`super::MAX_TOPIC_RULES`]), the actions performed at most [`MAX_ACTIONS_PER_MESSAGE`],
/// the republish hops at most [`MAX_REPUBLISH_HOPS`], the statement read under the bounds
/// [`super::rule_sql`] documents, and the record written is the published payload, which the
/// data plane has already weighed against its own quota.
pub async fn dispatch_published_message<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    topic: &str,
    payload: &[u8],
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    // The IAM store of this same engine, which decides what an action's role may do. `None`
    // is a deployment with no IAM to ask, which bars nothing.
    let iam = super::rule_action_role::iam_state(engine);
    // An action's target is reached in the Region the publish was signed for.
    let region = super::request_region(ctx.parts());
    let mut performed = 0_usize;
    // The messages still to evaluate: the published one, plus whatever a `republish` action
    // puts back in. Each is taken in turn, so a chain is followed breadth-first and the
    // bounds above hold over the whole chain rather than per message.
    let mut pending: VecDeque<Published> = VecDeque::from([Published {
        topic: topic.to_string(),
        payload: payload.to_vec(),
        hops: 0,
    }]);

    while let Some(published) = pending.pop_front() {
        // PASS 1 — under the guard, the rules this message is about.
        let selecting: Vec<Selecting> = {
            let store = state.lock();
            store
                .topic_rules
                .values()
                .filter(|rule| !rule.rule_disabled)
                .filter(|rule| {
                    rule.topic_pattern
                        .as_deref()
                        .is_some_and(|pattern| {
                            super::topic_filter_matches(pattern, &published.topic)
                        })
                })
                .map(|rule| Selecting {
                    rule_name: rule.rule_name.clone(),
                    rule_arn: rule.rule_arn.clone(),
                    sql: rule.sql.clone(),
                    actions: rule.actions.clone(),
                    error_action: rule.error_action.clone(),
                })
                .collect()
        };

        // PASS 2 — with NO guard held: the statement is read and the action's calls are made
        // outside the critical section, so no caller's statement is evaluated under a lock
        // over shared state and no lock is held across an await.
        for rule in selecting {
            // Reading a statement and evaluating it is CPU work on this worker, and a message
            // may select many rules: yielding first gives a cancellation or shutdown signal
            // somewhere to land between rules.
            ::tokio::task::yield_now().await;
            let message = match selected_message(&rule.sql, &published.topic, &published.payload) {
                Selected::Message(message) => message,
                Selected::NotSelected => continue,
                Selected::Undecided => {
                    ::tracing::debug!(
                        rule = %rule.rule_name,
                        "a topic rule's statement is not one this emulator's rules engine \
                         evaluates, so its actions were not performed"
                    );
                    continue;
                }
            };
            let mut failures: Vec<Failure> = Vec::new();
            for action in &rule.actions {
                let Some(performable) = Performed::of(action) else {
                    continue;
                };
                if performed >= MAX_ACTIONS_PER_MESSAGE {
                    ::tracing::warn!(
                        rule = %rule.rule_name,
                        performed,
                        "a published message matched more rule actions than this emulator \
                         performs for one message; the rest were not performed"
                    );
                    return;
                }
                performed += 1;
                if let Err(failure) = performable
                    .perform(
                        ctx,
                        &state,
                        &region,
                        iam.as_ref(),
                        &rule,
                        &published,
                        &message,
                        &mut pending,
                    )
                    .await
                {
                    ::tracing::debug!(
                        rule = %rule.rule_name,
                        action = failure.failed_action,
                        resource = %failure.failed_resource,
                        reason = %failure.error_message,
                        "a topic rule's action was refused, so the rule's errorAction is what \
                         reports it"
                    );
                    failures.push(failure);
                }
            }
            // "The action to take when an error occurs" (`TopicRulePayload$errorAction`): the
            // failures of THIS message against THIS rule are what it is handed, in one report,
            // and a rule carrying no error action reports them only in this emulator's log.
            if failures.is_empty() {
                continue;
            }
            let Some(error_action) = rule.error_action.as_ref() else {
                continue;
            };
            let Some(performable) = Performed::of(error_action) else {
                continue;
            };
            if performed >= MAX_ACTIONS_PER_MESSAGE {
                return;
            }
            performed += 1;
            let report = failure_report(&rule, &published, &failures);
            if let Err(failure) = performable
                .perform(
                    ctx,
                    &state,
                    &region,
                    iam.as_ref(),
                    &rule,
                    &published,
                    &report,
                    &mut pending,
                )
                .await
            {
                ::tracing::warn!(
                    rule = %rule.rule_name,
                    action = failure.failed_action,
                    reason = %failure.error_message,
                    "a topic rule's errorAction was itself refused, so its failure report \
                     reached nothing"
                );
            }
        }
    }
}

/// The rule-failure report an `errorAction` is handed: the rule that failed, the topic the
/// message was published to, the message's own bytes and what went wrong with each action.
///
/// `base64OriginalPayload` carries the published payload rather than the document the statement
/// selected, because the report is about a message that was NOT delivered: the bytes the
/// publisher sent are what a client has to re-drive, and they are the only form that survives a
/// payload which is no JSON document at all.
fn failure_report(rule: &Selecting, published: &Published, failures: &[Failure]) -> Vec<u8> {
    use ::base64::Engine as _;

    ::serde_json::json!({
        "ruleName": rule.rule_name,
        "topic": published.topic,
        "base64OriginalPayload":
            ::base64::engine::general_purpose::STANDARD.encode(&published.payload),
        "failures": failures
            .iter()
            .map(|failure| {
                ::serde_json::json!({
                    "failedAction": failure.failed_action,
                    "failedResource": failure.failed_resource,
                    "errorMessage": failure.error_message,
                })
            })
            .collect::<Vec<::serde_json::Value>>(),
    })
    .to_string()
    .into_bytes()
}

/// One action of a rule this engine performs, with the parameters it performs it from.
enum Performed<'a> {
    /// `Action$kinesis`: one [`KINESIS_PUT_RECORD_ACTION`] of the selected message onto the
    /// stream the action names, under its partition key.
    Kinesis(&'a KinesisAction),
    /// `Action$sqs`: one [`SQS_SEND_MESSAGE_ACTION`] of the selected message to the queue the
    /// action's `queueUrl` addresses.
    Sqs(&'a SqsAction),
    /// `Action$lambda`: one invocation of the function the action names, with the selected
    /// message as the invocation event.
    Lambda(&'a LambdaAction),
    /// `Action$republish`: the selected message put back into the broker on the action's own
    /// topic, where the rules whose filters cover it match it in turn.
    Republish(&'a RepublishAction),
    /// `Action$http`: one POST of the selected message to the endpoint URL the action names,
    /// made only while a topic rule destination admits that URL.
    Http(&'a HttpAction),
}

impl<'a> Performed<'a> {
    /// The action `action` carries, or `None` when it carries none this engine performs.
    /// `Action` is a structure of optional members, one per target service, of which a rule
    /// sets the one it performs; the members this engine leaves alone are named in the module
    /// documentation.
    fn of(action: &'a Action) -> Option<Self> {
        action
            .kinesis
            .as_ref()
            .map(Performed::Kinesis)
            .or_else(|| action.sqs.as_ref().map(Performed::Sqs))
            .or_else(|| action.lambda.as_ref().map(Performed::Lambda))
            .or_else(|| action.republish.as_ref().map(Performed::Republish))
            .or_else(|| action.http.as_ref().map(Performed::Http))
    }

    /// Perform the action for one selected message.
    ///
    /// `Err` is the action's FAILURE — what the rule's `errorAction` reports. An action this
    /// engine does not perform at all is `Ok`: that is this emulator's limit rather than the
    /// service's answer, so it manufactures no error report.
    #[allow(clippy::too_many_arguments)]
    async fn perform(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        state: &::engine::ServiceState<super::ComAmazonawsIotAwsiotserviceState>,
        region: &str,
        iam: Option<&::engine::ServiceState<IamState>>,
        rule: &Selecting,
        published: &Published,
        message: &[u8],
        pending: &mut VecDeque<Published>,
    ) -> Result<(), Failure> {
        match self {
            Self::Kinesis(action) => {
                put_record(ctx, region, iam, &rule.rule_name, action, message).await
            }
            Self::Sqs(action) => {
                send_message(ctx, region, iam, &rule.rule_name, action, message).await
            }
            Self::Lambda(action) => invoke_function(ctx, region, rule, action, message).await,
            Self::Republish(action) => {
                republish(region, iam, rule, published, action, message, pending)
            }
            Self::Http(action) => {
                post_to_endpoint(ctx, state, &rule.rule_name, action, message).await
            }
        }
    }
}

/// Invoke the function a `lambda` action names with the selected message as its event, as the
/// rule whose ARN narrows the grant that admits it ([`lambda_action`]).
async fn invoke_function(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    rule: &Selecting,
    action: &LambdaAction,
    message: &[u8],
) -> Result<(), Failure> {
    lambda_action::invoke(ctx, region, &rule.rule_arn, action, message)
        .await
        .map_err(|error_message| Failure {
            failed_action: "LambdaAction",
            failed_resource: action.function_arn.clone(),
            error_message,
        })
}

/// Put the selected message back into the broker on a `republish` action's own topic, under the
/// action's role: "publish to another MQTT topic" (`Action$republish`) makes the message a NEW
/// broker message, which the rules whose `FROM` clauses cover that topic then match — so a
/// two-hop chain reaches its second rule.
///
/// The role is what decides whether the republish happens at all: the action names "the ARN of
/// the IAM role that grants access", and the call it grants is [`IOT_PUBLISH_ACTION`] on the
/// topic the action names, so a role that does not admit this service principal or does not
/// grant that publish is a republish REFUSED — which the rule's `errorAction` reports.
///
/// `qos` ("the Quality of Service (QoS) level to use when republishing messages") decides how
/// the broker guarantees delivery to SUBSCRIBERS, which this emulator has none of, so it
/// changes nothing about which rules then match the republished message.
///
/// Nothing is republished for a topic carrying a substitution template, which this engine does
/// not render, nor past [`MAX_REPUBLISH_HOPS`]; neither is a failure of the action.
#[allow(clippy::too_many_arguments)]
fn republish(
    region: &str,
    iam: Option<&::engine::ServiceState<IamState>>,
    rule: &Selecting,
    published: &Published,
    action: &RepublishAction,
    message: &[u8],
    pending: &mut VecDeque<Published>,
) -> Result<(), Failure> {
    if action.topic.contains(SUBSTITUTION_TEMPLATE_OPENING) {
        ::tracing::debug!(
            rule = %rule.rule_name,
            "a republish rule action's topic is not one this emulator's rules engine renders \
             as the topic the action means, so nothing was republished"
        );
        return Ok(());
    }
    let topic_arn = ::emulator_server::emulator_arn(
        IOT_ARN_NAMESPACE,
        region,
        &format!("{TOPIC_RESOURCE_TYPE}/{}", action.topic),
    );
    let barred = iam.is_some_and(|iam| {
        role_bars_action(
            &iam.lock(),
            &action.role_arn,
            SERVICE_PRINCIPAL,
            IOT_PUBLISH_ACTION,
            &topic_arn,
        )
    });
    if barred {
        return Err(Failure {
            failed_action: "RepublishAction",
            failed_resource: topic_arn.clone(),
            error_message: role_refusal(&action.role_arn, IOT_PUBLISH_ACTION, &topic_arn),
        });
    }
    if published.hops >= MAX_REPUBLISH_HOPS {
        ::tracing::warn!(
            rule = %rule.rule_name,
            hops = published.hops,
            "a published message was republished more times than this emulator carries one \
             message through; it was not republished again"
        );
        return Ok(());
    }
    pending.push_back(Published {
        topic: action.topic.clone(),
        payload: message.to_vec(),
        hops: published.hops + 1,
    });
    Ok(())
}

/// POST one selected message to the endpoint an `http` action names, if a topic rule
/// destination admits that URL.
///
/// The GATE is the whole point of a topic rule destination: "the destination must be
/// confirmed prior to use" (`CreateTopicRuleDestination`), and traffic is allowed only while
/// a destination that admits the action's URL is `ENABLED`
/// ([`super::topic_rule_destinations::http_action_is_allowed`]) — so an action whose endpoint
/// has not been confirmed and enabled delivers NOTHING, which is the only observable that
/// guarantee has. The verdict is read under the store's guard and the guard is released
/// before the POST: no lock is held across an await.
///
/// Nothing is delivered either for an action this engine does not perform at all
/// ([`super::topic_rule_destinations::http_action_unperformable`]), and an endpoint that
/// refuses the request or is not there is a message the action FAILED to deliver — which is
/// what the rule's `errorAction` reports, and which AWS does not surface to the publisher
/// either.
///
/// The request is made over the emulator this publish is being served by and nothing else
/// ([`super::topic_rule_destinations::InProcessTransport`]): the action's URL is matched
/// against the services this emulator serves ITSELF, so no socket is opened, no host name is
/// resolved and no address off this process is reachable.
async fn post_to_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<super::ComAmazonawsIotAwsiotserviceState>,
    rule_name: &str,
    action: &HttpAction,
    message: &[u8],
) -> Result<(), Failure> {
    if let Some(reason) = http_action_unperformable(action) {
        ::tracing::debug!(
            rule = %rule_name,
            reason,
            "an http rule action is not one this emulator's rules engine performs, so \
             nothing was delivered to its endpoint"
        );
        return Ok(());
    }
    let allowed = {
        let mut store = state.lock();
        // A destination the clock has already carried out of `IN_PROGRESS` is read at the
        // status it rests at, which is the status the gate is about.
        super::settle_destinations(
            &mut store.topic_rule_destinations,
            ::std::time::SystemTime::now(),
        );
        http_action_is_allowed(&store.topic_rule_destinations, action)
    };
    if !allowed {
        ::tracing::debug!(
            rule = %rule_name,
            "an http rule action's endpoint has no enabled topic rule destination, so \
             nothing was delivered to it"
        );
        return Ok(());
    }
    let delivered = super::topic_rule_destinations::deliver_to_endpoint(
        &super::topic_rule_destinations::InProcessTransport::of(ctx.callback()),
        action,
        message,
    )
    .await;
    delivered.map_err(|reason| Failure {
        failed_action: "HttpAction",
        failed_resource: action.url.clone(),
        error_message: reason.to_string(),
    })
}

/// Write one Kinesis record for the selected message, under the action's role: the record's
/// data is the message the statement selected, its key is [`partition_key`], and the stream is
/// the `streamName` the action names in this publish's Region — reached through the shared
/// in-process client, so the write never leaves this process.
///
/// Nothing is written when the role bars the call, and nothing when Kinesis refuses the record
/// (a stream the account does not hold, one not yet active); either is the action FAILING,
/// which is what the rule's `errorAction` reports and which AWS does not surface to the
/// publisher either.
async fn put_record(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    iam: Option<&::engine::ServiceState<IamState>>,
    rule_name: &str,
    action: &KinesisAction,
    message: &[u8],
) -> Result<(), Failure> {
    let stream_arn = ::emulator_server::emulator_arn(
        KINESIS_ARN_NAMESPACE,
        region,
        &format!("{STREAM_RESOURCE_TYPE}/{}", action.stream_name),
    );
    // The verdict is read under the IAM guard and the guard is released before the write: no
    // lock is held across an await.
    let barred = iam.is_some_and(|iam| {
        role_bars_action(
            &iam.lock(),
            &action.role_arn,
            SERVICE_PRINCIPAL,
            KINESIS_PUT_RECORD_ACTION,
            &stream_arn,
        )
    });
    if barred {
        return Err(Failure {
            failed_action: "KinesisAction",
            failed_resource: stream_arn.clone(),
            error_message: role_refusal(
                &action.role_arn,
                KINESIS_PUT_RECORD_ACTION,
                &stream_arn,
            ),
        });
    }
    let Some(partition_key) = partition_key(action) else {
        ::tracing::debug!(
            rule = %rule_name,
            "a kinesis rule action's partitionKey is not one this emulator's rules engine \
             renders as the value the action means, so no record was written"
        );
        return Ok(());
    };
    let written = ::service_common::kinesis_client::kinesis_client(ctx, region)
        .put_record()
        .stream_name(&action.stream_name)
        .data(::aws_sdk_kinesis::primitives::Blob::new(message))
        .partition_key(partition_key)
        .send()
        .await;
    written.map(|_| ()).map_err(|error| Failure {
        failed_action: "KinesisAction",
        failed_resource: stream_arn,
        error_message: refusal_of(&error),
    })
}

/// The partition key one record is written under: "the partition key" the action names, or a
/// key unique to this record when the action names NONE — the key decides the shard and
/// Kinesis takes no record without one, so each record of an action naming none gets its own,
/// which spreads them over the stream's shards.
///
/// `None` — an action this engine does not perform — for a key it cannot render as the value
/// the action means: one carrying a substitution template
/// ([`SUBSTITUTION_TEMPLATE_OPENING`]), whose value is computed from the message, and an EMPTY
/// one, which `PartitionKey` (`length 1..256`) is not a value of, so Kinesis would refuse the
/// record rather than take it under a key this engine chose.
fn partition_key(action: &KinesisAction) -> Option<String> {
    match action.partition_key.as_deref() {
        Some(key) if key.contains(SUBSTITUTION_TEMPLATE_OPENING) || key.is_empty() => None,
        Some(key) => Some(key.to_string()),
        None => Some(super::fresh_uuid()),
    }
}

/// Send one Amazon SQS message for the selected message, under the action's role: the body is
/// the message the statement selected, and the queue is the one the action's `queueUrl`
/// addresses — reached through the shared in-process client, so the send never leaves this
/// process.
///
/// "Specifies whether to use Base64 encoding" (`SqsAction$useBase64`): with it set the body is
/// the base64 of the selected message, and otherwise the message's own bytes as the text of a
/// queue message — which is what an action leaving the member out sends.
///
/// Nothing is sent when the role bars the call, and nothing when Amazon SQS refuses the message
/// (a queue the account does not hold, a body its own quota refuses); either is the action
/// FAILING, which is what the rule's `errorAction` reports and which AWS does not surface to
/// the publisher either.
async fn send_message(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    iam: Option<&::engine::ServiceState<IamState>>,
    rule_name: &str,
    action: &SqsAction,
    message: &[u8],
) -> Result<(), Failure> {
    if action.queue_url.contains(SUBSTITUTION_TEMPLATE_OPENING) {
        ::tracing::debug!(
            rule = %rule_name,
            "an sqs rule action's queueUrl is not one this emulator's rules engine renders \
             as the queue the action means, so no message was sent"
        );
        return Ok(());
    }
    // The resource the role is granted the send on: the queue the action's URL addresses.
    let Some(queue_arn) = ::service_common::sqs_client::queue_arn_of_url(region, &action.queue_url)
    else {
        ::tracing::debug!(
            rule = %rule_name,
            "an sqs rule action's queueUrl addresses no queue, so no message was sent"
        );
        return Ok(());
    };
    // The verdict is read under the IAM guard and the guard is released before the send: no
    // lock is held across an await.
    let barred = iam.is_some_and(|iam| {
        role_bars_action(
            &iam.lock(),
            &action.role_arn,
            SERVICE_PRINCIPAL,
            SQS_SEND_MESSAGE_ACTION,
            &queue_arn,
        )
    });
    if barred {
        return Err(Failure {
            failed_action: "SqsAction",
            failed_resource: queue_arn.clone(),
            error_message: role_refusal(&action.role_arn, SQS_SEND_MESSAGE_ACTION, &queue_arn),
        });
    }
    let body = if action.use_base64.unwrap_or(false) {
        use ::base64::Engine as _;

        ::base64::engine::general_purpose::STANDARD.encode(message)
    } else {
        String::from_utf8_lossy(message).into_owned()
    };
    let sent = ::service_common::sqs_client::sqs_client(ctx, region)
        .send_message()
        .queue_url(&action.queue_url)
        .message_body(body)
        .send()
        .await;
    sent.map(|_| ()).map_err(|error| Failure {
        failed_action: "SqsAction",
        failed_resource: queue_arn,
        error_message: refusal_of(&error),
    })
}

/// What a refusal by the action's ROLE says, as the rule-failure report carries it: the
/// identity-based sentence AWS words for a principal whose own documents do not allow the call
/// ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]), naming the
/// role the action asked this service to act as, the call it asked for and the target it asked
/// for. The one sentence serves every action that names a role, so two of them cannot word the
/// same refusal differently.
fn role_refusal(role_arn: &str, action: &str, resource: &str) -> String {
    ::service_common::iam_authorization::not_authorized_by_identity_policy(
        role_arn,
        action,
        resource,
        &::service_common::iam_authorization::no_identity_policy_allows(action),
    )
}

/// What a target service's refusal of an action's call SAYS, as the rule-failure report carries
/// it: that service's own message where it worded one, and the error code it answered where it
/// did not — never a sentence made up here, because the failure a client reads has to be the
/// one the target really gave.
fn refusal_of<E, R>(error: &::aws_smithy_runtime_api::client::result::SdkError<E, R>) -> String
where
    ::aws_smithy_runtime_api::client::result::SdkError<E, R>:
        ::aws_smithy_types::error::metadata::ProvideErrorMetadata,
{
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata as _;

    error
        .message()
        .map(str::to_string)
        .or_else(|| error.code().map(str::to_string))
        .unwrap_or_else(|| UNWORDED_REFUSAL.to_string())
}

/// What the report says for a refusal the target service worded NOTHING about — neither a
/// message nor an error code — which is the one case there is nothing of its own to carry.
const UNWORDED_REFUSAL: &str = "the action's target refused the call";
