// SPDX-License-Identifier: CC0-1.0

//! What a target's ARN says about the delivery to it: which kind of resource it
//! names, which service's API a handover therefore calls, and what that call needs.
//!
//! The classification is read off the ARN alone — its vendor namespace, its resource
//! type and the shape of its resource field — so a target is decided by what it
//! names and never by the event being delivered. A target this emulator has no
//! invocation for classifies as nothing, which is what keeps a delivery from being
//! invented for it.
//!
//! Split out of the parent for length: it is the one self-contained concern there,
//! and nothing of the dispatch passes depends on how it reaches its verdict.

use super::*;

impl TargetKind {
    /// The kind `arn` names, or `None` when this emulator has no invocation for
    /// the resource it names — in which case nothing is handed over and nothing
    /// is invented in its place.
    pub(super) fn of(arn: &str) -> Option<Self> {
        let parsed = ::service_common::arn::parse(arn)?;
        match parsed.vendor {
            // A queue's ARN names the queue directly: a resource field carrying
            // a type separator is not one, so it addresses no queue.
            SQS_ARN_NAMESPACE
                if !parsed.resource.contains('/') && !parsed.resource.contains(':') =>
            {
                Some(Self::Queue)
            }
            LAMBDA_ARN_NAMESPACE if parsed.resource_type() == FUNCTION_RESOURCE_TYPE => {
                Some(Self::Function)
            }
            // A topic's ARN, like a queue's, names the topic directly.
            SNS_ARN_NAMESPACE
                if !parsed.resource.contains('/') && !parsed.resource.contains(':') =>
            {
                Some(Self::Topic)
            }
            STATES_ARN_NAMESPACE if parsed.resource_type() == STATE_MACHINE_RESOURCE_TYPE => {
                Some(Self::StateMachine)
            }
            // A data stream's ARN names the stream directly (`stream/<name>`): a
            // resource carrying a further segment addresses something within a
            // stream (a consumer) rather than the stream, so no record is written
            // for it.
            KINESIS_ARN_NAMESPACE
                if ::service_common::arn::resource_id_of_type(arn, STREAM_RESOURCE_TYPE)
                    .is_some_and(|name| !name.is_empty() && !name.contains(['/', ':'])) =>
            {
                Some(Self::Stream)
            }
            // This service's own resource: an API destination, whose ARN carries
            // the destination's name and its unique id
            // (`crate::state::api_destination_arn`).
            crate::state::ARN_NAMESPACE
                if parsed.resource_type() == API_DESTINATION_RESOURCE_TYPE =>
            {
                Some(Self::ApiDestination)
            }
            // A workflow's ARN names the workflow directly (`workflow/<name>`):
            // a resource carrying a further segment addresses something within a
            // workflow (one of its runs) rather than the workflow, so no event
            // is handed to it.
            GLUE_ARN_NAMESPACE
                if ::service_common::arn::resource_id_of_type(arn, WORKFLOW_RESOURCE_TYPE)
                    .is_some_and(|name| !name.is_empty() && !name.contains(['/', ':'])) =>
            {
                Some(Self::Workflow)
            }
            // A pipeline's ARN names the pipeline directly, the way a queue's and a
            // topic's do (`arn:<partition>:codepipeline:<region>:<account>:<name>`): a
            // resource field carrying a type separator addresses one of that service's
            // compound resources (an action type, a webhook) rather than a pipeline, so
            // no execution is started for it.
            CODEPIPELINE_ARN_NAMESPACE
                if !parsed.resource.is_empty()
                    && !parsed.resource.contains('/')
                    && !parsed.resource.contains(':') =>
            {
                Some(Self::Pipeline)
            }
            // A log group's ARN names the group, in either of the two forms that
            // name it (`crate::state::log_group_target::log_group_name`): a
            // resource addressing something WITHIN a group (one of its streams)
            // is not a group, so no log event is written for it.
            crate::state::log_group_target::LOGS_ARN_NAMESPACE
                if crate::state::log_group_target::log_group_name(arn).is_some() =>
            {
                Some(Self::LogGroup)
            }
            // A delivery stream's ARN names the stream directly
            // (`deliverystream/<name>`): a resource carrying a further segment
            // addresses something within a stream rather than the stream, so no
            // record is put into it.
            FIREHOSE_ARN_NAMESPACE
                if ::service_common::arn::resource_id_of_type(
                    arn,
                    DELIVERY_STREAM_RESOURCE_TYPE,
                )
                .is_some_and(|name| !name.is_empty() && !name.contains(['/', ':'])) =>
            {
                Some(Self::DeliveryStream)
            }
            // A cluster's ARN names the cluster directly (`cluster/<name>`): a
            // resource addressing something WITHIN a cluster (one of its tasks,
            // one of its services) is not a cluster, so no task is run for it.
            ECS_ARN_NAMESPACE if ecs_task::cluster_in_arn(arn).is_some() => Some(Self::EcsTask),
            // An invoke ARN names the api, the stage, the verb and the resource
            // path ONE request is made to: an ARN stating fewer of those names no
            // request at all ([`rest_api_stage::addressed_stage`]).
            rest_api_stage::EXECUTE_API_ARN_NAMESPACE
                if rest_api_stage::addressed_stage(arn).is_some() =>
            {
                Some(Self::RestApiStage)
            }
            // An AppSync target's ARN names the API's published GraphQL ENDPOINT, or the
            // API (and optionally one field of it) by id: an ARN naming fewer of those
            // names addresses no API, so no operation is executed for it
            // ([`app_sync::addressed`]).
            app_sync::APPSYNC_ARN_NAMESPACE if app_sync::addressed(arn).is_some() => {
                Some(Self::GraphqlApiField)
            }
            _ => None,
        }
    }

    /// The IAM action the invocation of this target kind performs, which is what
    /// the target's role has to be allowed to do.
    pub(super) fn action(self) -> &'static str {
        match self {
            Self::Queue => "sqs:SendMessage",
            Self::Function => "lambda:InvokeFunction",
            Self::Topic => "sns:Publish",
            Self::StateMachine => "states:StartExecution",
            Self::Stream => "kinesis:PutRecord",
            Self::ApiDestination => "events:InvokeApiDestination",
            // The action AWS requires of the role a Glue workflow target names,
            // and the one no operation of the Glue model serves: the ingress
            // EventBridge notifies a workflow's EVENT trigger through.
            Self::Workflow => "glue:notifyEvent",
            Self::Pipeline => "codepipeline:StartPipelineExecution",
            Self::LogGroup => crate::state::log_group_target::PUT_LOG_EVENTS_ACTION,
            Self::DeliveryStream => "firehose:PutRecord",
            Self::EcsTask => crate::state::ecs_target::RUN_TASK_ACTION,
            Self::RestApiStage => rest_api_stage::INVOKE_ACTION,
            Self::GraphqlApiField => app_sync::GRAPHQL_ACTION,
        }
    }

    /// The RESOURCE the action [`Self::action`] names is performed ON, which is
    /// what a grant in force on the target's role has to cover.
    ///
    /// The target's own ARN for every kind but one: an `ecs:RunTask` is performed
    /// on the TASK DEFINITION the run names — the resource type Amazon ECS
    /// publishes for that action — with the cluster the task is placed in carried
    /// as a condition key instead ([`ecs_task::run_resource`]).
    pub(super) fn action_resource<'a>(self, handover: &Handover<'a>) -> &'a str {
        match self {
            Self::EcsTask => ecs_task::run_resource(handover),
            _ => handover.target.arn.as_str(),
        }
    }

    /// Why the policies in force on the target's role bar this invocation, or
    /// `None` when nothing of the kind can be shown.
    ///
    /// The shared decision for every kind AWS invokes under that role
    /// ([`role_refusal`]), which an ECS task run takes with the cluster it places
    /// into published as a condition key
    /// ([`ecs_task::run_authorization_refusal`]) and which an invoke of an API
    /// Gateway stage REPORTS as the answer the endpoint gives an identity that may
    /// not invoke it ([`rest_api_stage::invoke_authorization_refusal`]) — as an
    /// AppSync field's execution reports it as the answer that API's endpoint gives
    /// such an identity ([`app_sync::invoke_authorization_refusal`]).
    pub(super) fn role_bars(
        self,
        stores: &Stores<'_>,
        role_arn: Option<&str>,
        handover: &Handover<'_>,
        attempt: &crate::state::DeliveryAttempt<'_>,
    ) -> Option<(String, String)> {
        let iam = stores.iam;
        match self {
            Self::EcsTask => ecs_task::run_authorization_refusal(iam, role_arn, handover, attempt),
            Self::RestApiStage => rest_api_stage::invoke_authorization_refusal(
                iam,
                role_arn,
                handover,
                attempt,
                stores.endpoint,
            ),
            Self::GraphqlApiField => {
                app_sync::invoke_authorization_refusal(stores, role_arn, handover, attempt)
            }
            _ => role_refusal(iam, role_arn, handover.rule_arn, attempt),
        }
    }

    /// Make the invocation, and answer what came of it.
    ///
    /// A target service that answers `NotImplemented` has not refused the
    /// delivery — nobody has written that operation in this emulator yet — so it
    /// is recorded as NOT ATTEMPTED rather than counted as a failed invocation:
    /// dead-lettering an event for a gap in the emulator would report a failure
    /// that AWS never produces.
    ///
    /// `role_grants` is what the ROLE the target names says about this call, for
    /// the kinds whose call AWS otherwise governs by the target's own
    /// resource-based policy ([`role_grants_call`]): a delivery made as a role
    /// that may make the call is admitted by that role, so the target's own
    /// document is not what decides it and is not read.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn invoke(
        self,
        callback: &smithy_server_runtime::EmbeddedEmulator,
        clients: &mut Clients,
        stores: &Stores<'_>,
        region: &str,
        handover: &Handover<'_>,
        body: &str,
        spent: &mut Spent,
        budget: &mut DeliveryBudget,
        sends: &mut QueueSends,
        role_grants: bool,
    ) -> Option<crate::state::InvocationOutcome> {
        let target = handover.target;
        let arn = target.arn.as_str();
        // The target's OWN Region — the ARN's — which is not necessarily the one
        // this request was signed for, falling back to it when the ARN names no
        // Region label.
        let region = ::service_common::arn::region_of(arn, self.vendor()).unwrap_or(region);
        Some(match self {
            // A queue target's message is HELD for the batch its queue's messages
            // of this request go out in ([`queue_sends`]), once the documents in
            // force over that queue have admitted it: `None` out of this
            // operation is a delivery that is going to be made and recorded from
            // what that batch answered, not one that was skipped.
            Self::Queue => {
                match queue_sends::hand_over(
                    callback,
                    clients,
                    stores,
                    region,
                    handover,
                    body,
                    sends,
                    role_grants,
                )
                .await
                {
                    queue_sends::Handled::Held => return None,
                    queue_sends::Handled::Outcome(outcome) => outcome,
                }
            }
            Self::Function => {
                // One client for both calls this delivery makes on the function,
                // for the reason the queue branch above gives.
                let client = clients.lambda(callback, region).clone();
                // The FUNCTION's own resource-based policy decides this delivery
                // before anything is invoked ([`function_delivery_refusal`]),
                // together with the ROLE the target names where it grants the
                // invocation too — and an explicit deny in that document still
                // refuses whatever the role allows
                // ([`admitted_by_target_or_role`]). It is read ONCE per request
                // per function ([`Clients::function_policy`]), for the reason the
                // queue branch above gives.
                let policy = clients.function_policy(&client, arn).await;
                if let Some(refusal) =
                    function_delivery_refusal(policy, arn, self.action(), handover, role_grants)
                {
                    return Some(refusal);
                }
                outcome(
                    client
                        // "Event: invoke the function asynchronously" — which is
                        // how EventBridge invokes a function target: the rule
                        // does not wait for the function's answer.
                        .invoke()
                        .function_name(arn)
                        .invocation_type(::aws_sdk_lambda::types::InvocationType::Event)
                        .payload(::aws_sdk_lambda::primitives::Blob::new(body.as_bytes()))
                        .send()
                        .await,
                )
            }
            Self::Topic => {
                // One client for both calls this delivery makes on the topic,
                // for the reason the queue branch above gives.
                let client = clients.sns(callback, region).clone();
                // The TOPIC's own configuration decides this delivery before
                // anything is published: its access policy
                // ([`topic_delivery_refusal`]) and, for an encrypted topic, the
                // policy of the key its messages are encrypted under
                // ([`topic_key_refusal`]). The attributes are read ONCE per
                // request per topic ([`Clients::topic_attributes`]), for the
                // reason the queue branch above gives.
                let attributes = clients.topic_attributes(&client, arn).await;
                if let Some(refusal) = topic_delivery_refusal(
                    attributes.as_ref(),
                    arn,
                    self.action(),
                    handover,
                    role_grants,
                ) {
                    return Some(refusal);
                }
                if let Some(refusal) =
                    topic_key_refusal(stores.kms, attributes.as_ref(), region, arn, handover)
                {
                    return Some(refusal);
                }
                outcome(client.publish().topic_arn(arn).message(body).send().await)
            }
            Self::StateMachine => outcome(
                clients
                    .sfn(callback, region)
                    .start_execution()
                    .state_machine_arn(arn)
                    // The execution's input is the whole event envelope, and the
                    // execution is NAMED after the event that started it: an
                    // execution name "must be unique for your Amazon Web
                    // Services account, Region, and state machine"
                    // (`StartExecution$name`), and EventBridge mints one per
                    // INVOCATION from the event id and a UUID of its own
                    // ([`state_machine_execution_name`]) — so the event id is
                    // the join key between the event and its execution while two
                    // targets naming two machines, and one event delivered
                    // twice, each get an execution of their own.
                    .name(state_machine_execution_name(handover.event_id))
                    .input(body)
                    .send()
                    .await,
            ),
            Self::Stream => {
                // Resolving the key reads the delivered text back as a document
                // and names a place in it, which is CPU work on this worker:
                // yielding first gives a cancellation or shutdown signal
                // somewhere to land before it starts.
                ::tokio::task::yield_now().await;
                let partition_key = kinesis_partition_key(
                    target.partition_key_path.as_deref(),
                    handover.event_id,
                    body,
                );
                outcome(
                    clients
                        .kinesis(callback, region)
                        .put_record()
                        .stream_arn(arn)
                        // The record's data is the whole event envelope, and its
                        // partition key is what decides the shard the event goes
                        // to.
                        .data(::aws_sdk_kinesis::primitives::Blob::new(body.as_bytes()))
                        .partition_key(partition_key)
                        .send()
                        .await,
                )
            }
            Self::ApiDestination => {
                // The stored `InvocationEndpoint` is a caller's text, and it is
                // NOT a host this process connects to: the invocation is made
                // over the in-process emulator transport
                // (`OperationContext::callback().http_connector()`, see
                // [`invoke_api_destination`]), which routes the request to a
                // service this emulator serves itself. No socket is opened, no
                // name is resolved and nothing outside this process can be
                // reached, so the endpoint's host, scheme and address range
                // decide nothing but which emulated service claims it.
                invoke_api_destination(callback, handover, body, spent, budget).await
            }
            // A Glue workflow is reached through the store of the Glue service
            // in this SAME engine rather than through a client: the ingress
            // EventBridge uses, `glue:notifyEvent`, is served by no operation of
            // the Glue model, so there is no API call to make — the event is
            // counted against the workflow's EVENT trigger where Glue's own
            // readers read it.
            Self::Workflow => notify_workflow(stores.glue, region, arn),
            Self::Pipeline => {
                // The pipeline is named by the resource segment of its ARN, which is
                // what `StartPipelineExecution$name` takes.
                let name = ::service_common::arn::parse(arn).map(|parsed| parsed.resource);
                let Some(name) = name.filter(|name| !name.is_empty()) else {
                    return Some(crate::state::InvocationOutcome::NotAttempted(
                        "the target ARN names no pipeline".to_string(),
                    ));
                };
                outcome(
                    clients
                        .codepipeline(callback, region)
                        .start_pipeline_execution()
                        .name(name)
                        // `clientRequestToken` is "the system-generated unique ID used to
                        // identify a unique execution request" and is an idempotency
                        // token, so the delivery carries the one identifier of this
                        // handover that is unique already — the event's id. One event
                        // delivered twice is then one execution rather than two, which is
                        // what makes the delivery idempotent (the state-machine target's
                        // rule, in the member that service takes it in).
                        .client_request_token(handover.event_id)
                        .send()
                        .await,
                )
            }
            Self::LogGroup => {
                // Resolving the record reads the delivered text back as a
                // document, which is CPU work on this worker: yielding first
                // gives a cancellation or shutdown signal somewhere to land
                // before it starts, as the stream branch above does.
                ::tokio::task::yield_now().await;
                deliver_log_event(
                    callback,
                    clients,
                    stores,
                    region,
                    arn,
                    self.action(),
                    handover,
                    body,
                    spent,
                )
                .await
            }
            Self::DeliveryStream => {
                // `PutRecord` addresses the stream by NAME
                // (`PutRecordInput$DeliveryStreamName`), which is the resource
                // segment of its ARN.
                let name = ::service_common::arn::resource_id_of_type(
                    arn,
                    DELIVERY_STREAM_RESOURCE_TYPE,
                );
                let Some(name) = name.filter(|name| !name.is_empty()) else {
                    return Some(crate::state::InvocationOutcome::NotAttempted(
                        "the target ARN names no delivery stream".to_string(),
                    ));
                };
                // The record's data is what the target receives, as the bytes
                // the stream then delivers to its own destination: "the data
                // blob … is the data that the producer sends to Firehose"
                // (`Record$Data`).
                let record = ::aws_sdk_firehose::types::Record::builder()
                    .data(::aws_sdk_firehose::primitives::Blob::new(body.as_bytes()))
                    .build();
                match record {
                    Ok(record) => outcome(
                        clients
                            .firehose(callback, region)
                            .put_record()
                            .delivery_stream_name(name)
                            .record(record)
                            .send()
                            .await,
                    ),
                    Err(error) => crate::state::InvocationOutcome::NotAttempted(format!(
                        "the record this event would be put as could not be built: {error}"
                    )),
                }
            }
            Self::EcsTask => {
                // Reading the delivered text back as the run's overrides is CPU
                // work on this worker: yielding first gives a cancellation or
                // shutdown signal somewhere to land before it starts, as the
                // stream branch above does.
                ::tokio::task::yield_now().await;
                ecs_task::run(callback, clients, stores, region, handover, body).await
            }
            Self::RestApiStage => {
                // The stage is reached over the IN-PROCESS emulator transport, the
                // way an api destination's endpoint is: the invoke url names the
                // api this emulator holds rather than a name anything resolves, so
                // no socket is opened and nothing outside this process is reached.
                rest_api_stage::invoke_rest_api_stage(
                    callback,
                    stores.endpoint,
                    region,
                    handover,
                    body,
                    spent,
                    budget,
                )
                .await
            }
            Self::GraphqlApiField => {
                // The API's own `GRAPHQL` endpoint EXECUTES the operation, in this
                // same process: that endpoint is a socket the AppSync service
                // serves per API rather than a route of this emulator's front
                // door, so the request is handed to the very code it serves and no
                // socket is opened ([`app_sync`]).
                app_sync::execute_graphql_operation(callback, stores, region, handover, body).await
            }
        })
    }

    /// The ARN namespace of the service this kind of target lives in.
    pub(super) fn vendor(self) -> &'static str {
        match self {
            Self::Queue => SQS_ARN_NAMESPACE,
            Self::Function => LAMBDA_ARN_NAMESPACE,
            Self::Topic => SNS_ARN_NAMESPACE,
            Self::StateMachine => STATES_ARN_NAMESPACE,
            Self::Stream => KINESIS_ARN_NAMESPACE,
            Self::ApiDestination => crate::state::ARN_NAMESPACE,
            Self::Workflow => GLUE_ARN_NAMESPACE,
            Self::Pipeline => CODEPIPELINE_ARN_NAMESPACE,
            Self::LogGroup => crate::state::log_group_target::LOGS_ARN_NAMESPACE,
            Self::DeliveryStream => FIREHOSE_ARN_NAMESPACE,
            Self::EcsTask => ECS_ARN_NAMESPACE,
            Self::RestApiStage => rest_api_stage::EXECUTE_API_ARN_NAMESPACE,
            Self::GraphqlApiField => app_sync::APPSYNC_ARN_NAMESPACE,
        }
    }
}
