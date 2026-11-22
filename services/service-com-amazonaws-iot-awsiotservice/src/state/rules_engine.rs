// SPDX-License-Identifier: CC0-1.0

//! The AWS IoT RULES ENGINE: what a message published to a topic DOES to the topic rules
//! this account holds.
//!
//! A rule's `sql` selects the messages it is about and its `actions` are what AWS IoT then
//! does with each of them, so a message published to a topic a stored rule's `FROM` clause
//! covers is evaluated against that rule and each of its actions is performed. Driven from
//! the data plane's `Publish` ([`dispatch_published_message`]) and from the MESSAGE BROKER's
//! own MQTT wire, whose publications reach it through the sink
//! [`broker_publications`] installs ([`dispatch_broker_publication`]); none of it is reported
//! to the
//! publisher, so what a client observes is the effect on the action's TARGET — the record a
//! `kinesis` action wrote onto its stream, the event a `lambda` action handed its function.
//!
//! Three gates, in order: the rule's topic filter covers the topic
//! ([`super::topic_filter_matches`], the reading `ListTopicRules` also uses) and the rule is
//! not disabled — unless the publication NAMED its rule, which AWS IoT routes to that rule
//! instead of matching the filters at all
//! ([`::service_common::iot_mqtt_broker::basic_ingest`]); the statement SELECTS the message ([`super::rule_sql`], which reads the
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
//! An action's PARAMETERS are not always literals: a value carrying `${…}` is a substitution
//! template, which AWS IoT renders from the message before it makes the action's call, so each
//! one is evaluated over the published message and the topic here too
//! ([`substitution_template`]) — the object key an `s3` action computes per message among them.
//!
//! What this engine cannot compute faithfully it does NOT perform: a statement it cannot
//! read, a condition it cannot decide, and a parameter whose template it cannot evaluate are
//! each left unperformed rather than performed on a guessed value. None of those
//! is a FAILURE of the action either, because they are this emulator's limits and not the
//! service's answer, so none of them manufactures an error report.
//!
//! The actions performed today are `kinesis`, "write data to an Amazon Kinesis stream", over
//! the emulator's own in-process transport
//! ([`::service_common::kinesis_client`] — no socket, and no part of the action is a
//! destination), `sqs`, "publish data to an Amazon SQS queue", over that same transport
//! ([`::service_common::sqs_client`]), `lambda`, "invoke a Lambda function", over that same
//! transport ([`lambda_action`]), `dynamoDBv2`, which writes "each attribute in an MQTT message
//! payload into a separate DynamoDB column", one item per selected message over that same
//! transport ([`dynamodb_action`]), `s3`, "write to an Amazon S3 bucket", one object holding the
//! selected message under the key the action renders ([`s3_action`]), `republish`, "publish to
//! another MQTT topic", which
//! re-injects the selected message into this same broker so the rules whose filters cover the
//! action's topic match it in turn ([`republish`]), and `http`, "send data to an HTTPS
//! endpoint", whose endpoint is a TOPIC RULE
//! DESTINATION: the message is POSTed to the action's `url` only while a destination that
//! admits that URL is `ENABLED` ([`super::topic_rule_destinations::http_action_is_allowed`]),
//! and nothing is sent otherwise — "the destination must be confirmed prior to use"
//! (`CreateTopicRuleDestination`), which is the gate the whole confirm/enable lifecycle
//! exists for. Every other member of `Action` is a target service with its own semantics (the
//! `dynamoDB` action's own key fields, operation verb and payload column, a Firehose record, an
//! OpenSearch document) and
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
    Action, DynamoDBv2Action, HttpAction, KinesisAction, LambdaAction, RepublishAction, S3Action,
    SqsAction,
};
use substitution_template::Parameters;

/// `Action$dynamoDBv2`: the item a rule's `dynamoDBv2` action writes for one selected message,
/// and the hop it is written over.
pub mod dynamodb_action;

/// What a publication the MESSAGE BROKER took is evaluated by: this engine, reached from the
/// broker's own MQTT wire.
pub mod broker_publications;

/// `Action$lambda`: the invocation a rule's `lambda` action makes, and the function
/// resource-policy grant that admits it.
pub mod lambda_action;

/// `Action$s3`: the object a rule's `s3` action stores for one selected message.
pub mod s3_action;

/// The `${…}` substitution templates an action's parameters carry, and what each is worth for
/// the message being acted on.
pub mod substitution_template;

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
    /// The rule this message was ADDRESSED to, and `None` for a message that is about
    /// whichever rules cover its topic.
    ///
    /// Set for a message a client sent to a rule BY NAME — the Basic Ingest path
    /// (`::service_common::iot_mqtt_broker::basic_ingest`) — which is what names the one rule
    /// it runs instead of the statements' own topic filters deciding. A `republish` action
    /// puts an ordinary message back in whatever carried it here, since the topic it
    /// republishes to is what the rules after it are matched against.
    addressed_rule: Option<String>,
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
/// The Region an action's target is reached in is the one the publish was signed for; a
/// publication that rode no signed request of its own ([`dispatch_broker_publication`]) names
/// its Region itself.
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
    let region = super::request_region(ctx.parts()).to_string();
    dispatch(
        engine,
        ctx,
        &region,
        Published {
            topic: topic.to_string(),
            payload: payload.to_vec(),
            hops: 0,
            addressed_rule: None,
        },
    )
    .await;
}

/// Evaluate one publication the MESSAGE BROKER took, in the Region the endpoint it was
/// published at belongs to.
///
/// The broker's own wire carries no signed request, so the Region is passed rather than read
/// off one, and a publication the client addressed to a rule BY NAME
/// (`Publication$addressed_rule`, the Basic Ingest path) is evaluated against that rule alone
/// — AWS IoT routes such a message to the named rule instead of matching the rules' own topic
/// filters, and the topic the statement reads is the one the broker handed over.
///
/// Everything else is [`dispatch_published_message`]: the same rules, the same statement
/// reading, the same actions and the same bounds.
pub async fn dispatch_broker_publication<
    Eng: ::std::marker::Send + ::std::marker::Sync + 'static,
>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    publication: ::service_common::iot_mqtt_broker::Publication,
) {
    dispatch(
        engine,
        ctx,
        region,
        Published {
            topic: publication.topic,
            payload: publication.payload,
            hops: 0,
            addressed_rule: publication.addressed_rule,
        },
    )
    .await;
}

/// Evaluate `message` and whatever a `republish` action of a rule it selects puts back in —
/// the one walk both entry points above run.
async fn dispatch<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    message: Published,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    // The IAM store of this same engine, which decides what an action's role may do. `None`
    // is a deployment with no IAM to ask, which bars nothing.
    let iam = super::rule_action_role::iam_state(engine);
    let mut performed = 0_usize;
    // The messages still to evaluate: the published one, plus whatever a `republish` action
    // puts back in. Each is taken in turn, so a chain is followed breadth-first and the
    // bounds above hold over the whole chain rather than per message.
    let mut pending: VecDeque<Published> = VecDeque::from([message]);

    while let Some(published) = pending.pop_front() {
        // PASS 1 — under the guard, the rules this message is about: the one it was ADDRESSED
        // to where it names one, and otherwise every enabled rule whose statement's topic
        // filter covers the topic. A rule is run only while it is enabled either way, since a
        // disabled rule is a rule that does nothing.
        let selecting: Vec<Selecting> = {
            let store = state.lock();
            store
                .topic_rules
                .values()
                .filter(|rule| !rule.rule_disabled)
                .filter(|rule| match published.addressed_rule.as_deref() {
                    Some(addressed) => rule.rule_name == addressed,
                    None => rule.topic_pattern.as_deref().is_some_and(|pattern| {
                        super::topic_filter_matches(pattern, &published.topic)
                    }),
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
        //
        // The parameters of every action of every rule are rendered against THIS message —
        // read once here, because a template is evaluated over the message the broker took
        // and the topic it was published to rather than over what a rule's statement selected
        // ([`substitution_template`]).
        let parameters = Parameters::of(&published.topic, &published.payload);
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
                        region,
                        iam.as_ref(),
                        &rule,
                        &published,
                        &parameters,
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
                    region,
                    iam.as_ref(),
                    &rule,
                    &published,
                    &parameters,
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
    /// `Action$dynamoDBv2`: one [`dynamodb_action::PUT_ITEM_ACTION`] of the selected message
    /// into the table the action names, one column per attribute of the message.
    DynamoDbV2(&'a DynamoDBv2Action),
    /// `Action$s3`: one [`s3_action::PUT_OBJECT_ACTION`] of the selected message into the
    /// bucket the action names, under the object key it renders.
    S3(&'a S3Action),
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
            .or_else(|| action.dynamo_d_bv2.as_ref().map(Performed::DynamoDbV2))
            .or_else(|| action.s3.as_ref().map(Performed::S3))
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
        parameters: &Parameters,
        message: &[u8],
        pending: &mut VecDeque<Published>,
    ) -> Result<(), Failure> {
        match self {
            Self::Kinesis(action) => {
                put_record(ctx, region, iam, &rule.rule_name, parameters, action, message).await
            }
            Self::Sqs(action) => {
                send_message(ctx, region, iam, &rule.rule_name, parameters, action, message).await
            }
            Self::Lambda(action) => invoke_function(ctx, region, rule, action, message).await,
            Self::DynamoDbV2(action) => {
                write_item(ctx, region, iam, &rule.rule_name, parameters, action, message).await
            }
            Self::S3(action) => {
                store_object(ctx, region, iam, &rule.rule_name, parameters, action, message).await
            }
            Self::Republish(action) => {
                republish(region, iam, rule, published, parameters, action, message, pending)
            }
            Self::Http(action) => {
                post_to_endpoint(ctx, state, &rule.rule_name, action, message).await
            }
        }
    }
}

/// Write one DynamoDB item for the selected message, under the action's role: the item is the
/// message itself — one column per attribute — and the table is the one the action's `putItem`
/// names in this publish's Region ([`dynamodb_action`]).
///
/// Nothing is written when the role bars the call, and nothing when DynamoDB refuses the item (a
/// table the account does not hold, one not yet active, a message carrying no value for the
/// table's key); either is the action FAILING, which is what the rule's `errorAction` reports
/// and which AWS does not surface to the publisher either.
#[allow(clippy::too_many_arguments)]
async fn write_item(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    iam: Option<&::engine::ServiceState<IamState>>,
    rule_name: &str,
    parameters: &Parameters,
    action: &DynamoDBv2Action,
    message: &[u8],
) -> Result<(), Failure> {
    let Some(table_name) = parameters.rendered(&action.put_item.table_name) else {
        unrenderable(rule_name, "dynamoDBv2", "putItem.tableName");
        return Ok(());
    };
    let table_arn = dynamodb_action::table_arn(region, &table_name);
    // The verdict is read under the IAM guard and the guard is released before the write: no
    // lock is held across an await.
    let barred = iam.is_some_and(|iam| {
        role_bars_action(
            &iam.lock(),
            &action.role_arn,
            SERVICE_PRINCIPAL,
            dynamodb_action::PUT_ITEM_ACTION,
            &table_arn,
        )
    });
    if barred {
        return Err(Failure {
            failed_action: dynamodb_action::FAILED_ACTION,
            failed_resource: table_arn.clone(),
            error_message: role_refusal(
                &action.role_arn,
                dynamodb_action::PUT_ITEM_ACTION,
                &table_arn,
            ),
        });
    }
    dynamodb_action::put_item(ctx, region, &table_name, message)
        .await
        .map_err(|error_message| Failure {
            failed_action: dynamodb_action::FAILED_ACTION,
            failed_resource: table_arn,
            error_message,
        })
}

/// Store one Amazon S3 object holding the selected message, under the action's role: the body is
/// the message the statement selected and the key is the one the action renders for this message
/// ([`s3_action`]).
///
/// Nothing is stored when the role bars the call, and nothing when Amazon S3 refuses the object
/// (a bucket the account does not hold, an ACL the bucket bars); either is the action FAILING,
/// which is what the rule's `errorAction` reports and which AWS does not surface to the publisher
/// either.
#[allow(clippy::too_many_arguments)]
async fn store_object(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    iam: Option<&::engine::ServiceState<IamState>>,
    rule_name: &str,
    parameters: &Parameters,
    action: &S3Action,
    message: &[u8],
) -> Result<(), Failure> {
    let Some(bucket) = parameters.rendered(&action.bucket_name) else {
        unrenderable(rule_name, "s3", "bucketName");
        return Ok(());
    };
    // "The object key" is the parameter AWS IoT most often computes per message, which is what a
    // rule writing one object per device or per day is written with.
    let Some(key) = parameters.rendered(&action.key) else {
        unrenderable(rule_name, "s3", "key");
        return Ok(());
    };
    let object_arn = s3_action::object_arn(&bucket, &key);
    // The verdict is read under the IAM guard and the guard is released before the write: no
    // lock is held across an await.
    let barred = iam.is_some_and(|iam| {
        role_bars_action(
            &iam.lock(),
            &action.role_arn,
            SERVICE_PRINCIPAL,
            s3_action::PUT_OBJECT_ACTION,
            &object_arn,
        )
    });
    if barred {
        return Err(Failure {
            failed_action: s3_action::FAILED_ACTION,
            failed_resource: object_arn.clone(),
            error_message: role_refusal(
                &action.role_arn,
                s3_action::PUT_OBJECT_ACTION,
                &object_arn,
            ),
        });
    }
    s3_action::put_object(ctx, region, &bucket, &key, action, message)
        .await
        .map_err(|error_message| Failure {
            failed_action: s3_action::FAILED_ACTION,
            failed_resource: object_arn,
            error_message,
        })
}

/// What the engine's log says about an action parameter whose substitution template this
/// emulator does not evaluate: the action is not performed at all, which is this emulator's own
/// limit rather than the service's answer, so it is no failure of the action
/// ([`substitution_template`]).
fn unrenderable(rule_name: &str, action: &str, parameter: &str) {
    ::tracing::debug!(
        rule = %rule_name,
        action,
        parameter,
        "a rule action's parameter carries a substitution template this emulator's rules \
         engine does not evaluate, so the action was not performed"
    );
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
/// Nothing is republished for a topic whose substitution template this engine does not evaluate,
/// nor past [`MAX_REPUBLISH_HOPS`]; neither is a failure of the action.
#[allow(clippy::too_many_arguments)]
fn republish(
    region: &str,
    iam: Option<&::engine::ServiceState<IamState>>,
    rule: &Selecting,
    published: &Published,
    parameters: &Parameters,
    action: &RepublishAction,
    message: &[u8],
    pending: &mut VecDeque<Published>,
) -> Result<(), Failure> {
    let Some(topic) = parameters.rendered(&action.topic) else {
        unrenderable(&rule.rule_name, "republish", "topic");
        return Ok(());
    };
    let topic_arn = ::emulator_server::emulator_arn(
        IOT_ARN_NAMESPACE,
        region,
        &format!("{TOPIC_RESOURCE_TYPE}/{topic}"),
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
        topic,
        payload: message.to_vec(),
        hops: published.hops + 1,
        addressed_rule: None,
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
    parameters: &Parameters,
    action: &KinesisAction,
    message: &[u8],
) -> Result<(), Failure> {
    let Some(stream_name) = parameters.rendered(&action.stream_name) else {
        unrenderable(rule_name, "kinesis", "streamName");
        return Ok(());
    };
    let stream_arn = ::emulator_server::emulator_arn(
        KINESIS_ARN_NAMESPACE,
        region,
        &format!("{STREAM_RESOURCE_TYPE}/{stream_name}"),
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
    let Some(partition_key) = partition_key(parameters, action) else {
        unrenderable(rule_name, "kinesis", "partitionKey");
        return Ok(());
    };
    let written = ::service_common::kinesis_client::kinesis_client(ctx, region)
        .put_record()
        .stream_name(&stream_name)
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

/// The partition key one record is written under: "the partition key" the action names, rendered
/// for this message, or a key unique to this record when the action names NONE — the key decides
/// the shard and Kinesis takes no record without one, so each record of an action naming none
/// gets its own, which spreads them over the stream's shards.
///
/// `None` — an action this engine does not perform — for a key it cannot render as the value the
/// action means: one whose substitution template it does not evaluate
/// ([`substitution_template`]), and an EMPTY one, which `PartitionKey` (`length 1..256`) is not a
/// value of, so Kinesis would refuse the record rather than take it under a key this engine
/// chose.
fn partition_key(parameters: &Parameters, action: &KinesisAction) -> Option<String> {
    match action.partition_key.as_deref() {
        Some(key) => parameters.rendered(key).filter(|key| !key.is_empty()),
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
    parameters: &Parameters,
    action: &SqsAction,
    message: &[u8],
) -> Result<(), Failure> {
    let Some(queue_url) = parameters.rendered(&action.queue_url) else {
        unrenderable(rule_name, "sqs", "queueUrl");
        return Ok(());
    };
    // The resource the role is granted the send on: the queue the action's URL addresses.
    let Some(queue_arn) = ::service_common::sqs_client::queue_arn_of_url(region, &queue_url) else {
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
        .queue_url(&queue_url)
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
/// SESSION this service performs the action as, the call it asked for and the target it asked
/// for. The one sentence serves every action that names a role, so two of them cannot word the
/// same refusal differently.
///
/// The principal is the session and not the role itself: an action names "the ARN of the IAM
/// role that grants access" to its target and AWS IoT acts AS that role, which is done by
/// assuming it — so the identity the refused call was made by is a session of the role
/// ([`::service_common::iam_role::assumed_role_session_arn`], the tree's one shaping of that
/// principal) and that is what the target service's access-denied sentence is about.
fn role_refusal(role_arn: &str, action: &str, resource: &str) -> String {
    ::service_common::iam_authorization::not_authorized_by_identity_policy(
        &::service_common::iam_role::assumed_role_session_arn(role_arn, ROLE_SESSION_NAME),
        action,
        resource,
        &::service_common::iam_authorization::no_identity_policy_allows(action),
    )
}

/// The name of the role SESSION the rules engine performs a rule's actions under, which the
/// principal of a refusal naming that session carries.
///
/// The role is the action's; the session NAME is the acting service's, and AWS IoT publishes no
/// name for the sessions its rules engine opens, so this is the emulator's own stable name for
/// them — the one part of the principal that is not read off the action's `roleArn`.
pub const ROLE_SESSION_NAME: &str = "AWSIoTRulesEngine";

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
