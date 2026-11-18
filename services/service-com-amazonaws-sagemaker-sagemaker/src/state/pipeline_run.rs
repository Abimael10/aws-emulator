// SPDX-License-Identifier: CC0-1.0

//! The PIPELINE RUN slice of this service's state: what a started execution DOES with the
//! steps its definition declares, and what each of those steps then reports.
//!
//! A pipeline execution is not one status with a span: it is the steps it performs, each
//! with its own status, its own instants and its own outcome, and the run's status follows
//! them. Two kinds of step are told apart here:
//!
//! 1. **A step this emulator PERFORMS.** A callback step's whole work is handing the task
//!    over to the caller's own worker: "when a callback step is run, the pipeline generates
//!    a callback token and includes the token in a message sent to Amazon Simple Queue
//!    Service" (`SendPipelineExecutionStepSuccess`'s own doc). So the run really sends that
//!    message to the queue the step names ([`CallbackDelivery`]), really records the token
//!    it minted on the step ([`CallbackGrant`]), and the step stays
//!    [`STEP_STATUS_EXECUTING`] — holding the run open — until that token is reported back
//!    with `SendPipelineExecutionStepSuccess` or `SendPipelineExecutionStepFailure`, which
//!    is what settles the step and then the run.
//! 2. **A step nothing here can perform.** Training, processing, transform, Lambda and the
//!    rest need a runtime this emulator does not have, so the run does NOT dispatch them:
//!    they are counted ([`PipelineStepDispatch::undispatched`]) and the clock settles the
//!    run `Failed` with [`crate::state::PIPELINE_EXECUTION_NOT_RUN`] as its reason, rather
//!    than reporting work that did not happen.
//!
//! A run whose role resolved to nothing dispatches NOTHING — the step would have been
//! performed AS that role — so no callback message is produced for it and the run carries
//! the role's own failure reason.

use super::*;
use crate::ops::types::OutputParameter;

/// `StepStatus`'s members a step run of this emulator reports
/// (`smithy_query(query="shape",shape="StepStatus")` answers `Starting`, `Executing`,
/// `Stopping`, `Stopped`, `Failed`, `Succeeded`). Named here so no handler compares a bare
/// string literal.
pub const STEP_STATUS_EXECUTING: &str = "Executing";
/// See [`STEP_STATUS_EXECUTING`]: the terminal status of a step whose work was reported
/// successful.
pub const STEP_STATUS_SUCCEEDED: &str = "Succeeded";
/// See [`STEP_STATUS_EXECUTING`]: the terminal status of a step whose work failed, or which
/// nothing performed.
pub const STEP_STATUS_FAILED: &str = "Failed";

/// The `Type` a callback step declares in a pipeline definition — the one step kind whose
/// work is performed HERE, because its work is a message on the caller's own queue.
pub const PIPELINE_CALLBACK_STEP_TYPE: &str = "Callback";

/// How many characters a `CallbackToken` carries: the model declares it `@length 10..10`
/// with `@pattern ^[a-zA-Z0-9]+$`
/// (`smithy_query(query="operation",operation="SendPipelineExecutionStepSuccess")`), so a
/// token outside that is one no caller could present back.
pub const CALLBACK_TOKEN_LENGTH: usize = 10;

/// The member of a callback message carrying the token the step was dispatched with, as
/// the Amazon SQS message SageMaker Pipelines sends spells it.
const CALLBACK_MESSAGE_TOKEN: &str = "token";

/// The member of a callback message carrying the step's own `Arguments` — what the worker
/// needs to do the work the step handed it.
const CALLBACK_MESSAGE_ARGUMENTS: &str = "arguments";

/// The member the ADDITIONAL message a stop sends carries, as `StopPipelineExecution`'s own
/// doc spells it: "the body of the SQS message contains a \"Status\" field which is set to
/// \"Stopping\"".
const CALLBACK_MESSAGE_STATUS: &str = "Status";

/// What that `Status` field is set to: the run is stopping, and the worker is to take
/// whatever action it needs and then report the step's outcome back.
pub const CALLBACK_STATUS_STOPPING: &str = "Stopping";

/// The token one callback step was dispatched with, and the queue the work was handed over
/// through.
///
/// The token is what a worker presents back to settle the step, so it is recorded ON the
/// step: a token that resolves to no step waiting for it settles nothing
/// ([`ComAmazonawsSagemakerSagemakerState::step_with_callback_token`]).
#[derive(Debug, Clone)]
pub struct CallbackGrant {
    pub token: String,
    pub queue_url: String,
    /// The output parameters the callback REPORTED when it settled the step, which is what
    /// `CallbackStepMetadata$OutputParameters` carries; empty while the step is still
    /// waiting, because nothing has reported any.
    pub output_parameters: Vec<OutputParameter>,
    /// The `ClientRequestToken` of the call that settled this step, so a RETRY of that very
    /// call "completes no more than one time" as the member documents: it answers what that
    /// call answered instead of being told the step is no longer waiting. `None` for a step
    /// still waiting, and for one settled by a call that presented no token — which has
    /// nothing to recognise a retry by.
    pub settled_by: Option<String>,
}

/// One step a run performed or is performing, as `ListPipelineExecutionSteps` reports it.
///
/// Only a DISPATCHED step has one of these: a step nothing performed has no run, no
/// instants and no attempt of its own (see this module's header).
#[derive(Debug, Clone)]
pub struct PipelineStepRun {
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub status: String,
    pub failure_reason: Option<String>,
    /// When the run dispatched this step, which is "the time that the step started
    /// executing" (`PipelineExecutionStep$StartTime`).
    pub start_time: SystemTime,
    /// When the step stopped executing; `None` while it still is.
    pub end_time: Option<SystemTime>,
    /// "The current attempt of the execution step" — one, because nothing here retries a
    /// step.
    pub attempt_count: i32,
    pub callback: Option<CallbackGrant>,
}

impl PipelineStepRun {
    /// Whether this step has stopped executing.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.status != STEP_STATUS_EXECUTING
    }

    /// Settle this step at `status` with `reason`, at the instant `now` the settling call
    /// reported it, remembering the `client_request_token` that call presented so a retry of
    /// it is recognised ([`CallbackGrant::settled_by`]).
    pub fn settle(
        &mut self,
        now: SystemTime,
        status: &str,
        reason: Option<String>,
        client_request_token: Option<String>,
    ) {
        self.status = status.to_string();
        self.failure_reason = reason;
        self.end_time = Some(now);
        if let Some(grant) = self.callback.as_mut() {
            grant.settled_by = client_request_token;
        }
    }

    /// Whether `client_request_token` is the one that settled this step — so the call
    /// presenting it is a RETRY of the call that settled it, which completes no more than
    /// once. A call presenting no token is never a retry.
    #[must_use]
    pub fn settled_by(&self, client_request_token: Option<&str>) -> bool {
        match (self.callback.as_ref().and_then(|grant| grant.settled_by.as_deref()), client_request_token) {
            (Some(held), Some(presented)) => held == presented,
            _ => false,
        }
    }
}

/// What a start owes for the steps one run is to perform: the step runs it dispatched, the
/// messages those dispatches have to deliver, and how many declared steps it dispatched
/// nothing for.
#[derive(Debug, Default)]
pub struct PipelineStepDispatch {
    pub runs: Vec<PipelineStepRun>,
    pub deliveries: Vec<CallbackDelivery>,
    /// The declared steps this run performs nothing for, because no runtime here performs
    /// their kind of work. They are what makes the run end `Failed` with
    /// [`crate::state::PIPELINE_EXECUTION_NOT_RUN`].
    pub undispatched: usize,
}

impl PipelineStepDispatch {
    /// The dispatch of a run that performs NONE of the `declared` steps it was to perform:
    /// nothing was dispatched and nothing is owed to any queue, so every one of them is
    /// counted undispatched. What a run whose role resolved to nothing does.
    #[must_use]
    pub fn nothing_dispatched(declared: usize) -> Self {
        Self {
            runs: Vec::new(),
            deliveries: Vec::new(),
            undispatched: declared,
        }
    }
}

/// One message a dispatched callback step owes the queue it hands its work over through.
#[derive(Debug, Clone)]
pub struct CallbackDelivery {
    pub step_name: String,
    /// The ADDRESS this emulator's own Amazon SQS takes the message at
    /// ([`callback_queue_address`]) — built from the Region and the queue the step's
    /// declared url names, never that url itself.
    pub queue_url: String,
    /// The message body, already rendered ([`callback_message`]), so the delivery carries
    /// no reference into the definition it was read from.
    pub message: String,
}

/// The longest declared `SqsQueueUrl` a callback step may hand its work over through: the
/// `String256` bound the member a step execution reports it under carries
/// (`smithy_query(query="shape",shape="CallbackStepMetadata")`). A definition may carry a
/// longer value — `PipelineDefinition` admits a megabyte — and a step whose queue could not
/// be reported is one no worker could be told about, so it is not dispatched.
pub const MAX_CALLBACK_QUEUE_URL_LEN: usize = 256;

/// The address this emulator's own Amazon SQS takes a message at for the queue `declared`
/// names, or `None` for a value that addresses no queue of this account at all.
///
/// A queue is ADDRESSED by its `/<account>/<name>` path — the one reading of a queue url
/// this workspace has ([`::service_common::sqs_client::queue_arn_of_url`]) takes both off
/// the value — and the url a delivery presents is built HERE from that pair and the Region.
/// So the scheme and the authority a pipeline definition wrote are DISCARDED rather than
/// used as an address: nothing a caller can put in a definition names a destination, and the
/// only queues a run can hand work to are the ones this emulator's own Amazon SQS holds.
#[must_use]
pub fn callback_queue_address(region: &str, declared: &str) -> Option<String> {
    if declared.len() > MAX_CALLBACK_QUEUE_URL_LEN {
        return None;
    }
    let arn = ::service_common::sqs_client::queue_arn_of_url(region, declared)?;
    // `<…>:<account>:<name>` — the two trailing fields of the ARN that reading answers.
    let mut fields = arn.rsplit(':');
    let name = fields.next().filter(|name| !name.is_empty())?;
    let account = fields.next().filter(|account| !account.is_empty())?;
    Some(format!(
        "https://sqs.{region}.amazonaws.com/{account}/{name}"
    ))
}

/// Dispatch the steps `declared` declares, at `now`, for a request addressing `region`.
///
/// A callback step whose declared queue addresses a queue of this account
/// ([`callback_queue_address`]) is dispatched: a token is minted for it, the step run is
/// recorded `Executing` and the message it owes that queue is rendered. A callback step that
/// names no such queue is dispatched for nothing — no worker could ever be told, let alone
/// answer — so its run is recorded FAILED at once with
/// [`callback_step_names_no_queue`]. Every other kind of step is left undispatched and
/// counted, because no runtime here performs its work.
#[must_use]
pub fn dispatch_pipeline_steps(
    declared: &[PipelineStepDeclaration],
    region: &str,
    now: SystemTime,
) -> PipelineStepDispatch {
    let mut dispatch = PipelineStepDispatch::default();
    for step in declared {
        let declared_url = match (step.step_type.as_str(), step.sqs_queue_url.as_deref()) {
            (PIPELINE_CALLBACK_STEP_TYPE, url) => url.unwrap_or_default(),
            _ => {
                dispatch.undispatched += 1;
                continue;
            }
        };
        let token = mint_callback_token();
        let mut run = PipelineStepRun {
            name: step.name.clone(),
            display_name: step.display_name.clone(),
            description: step.description.clone(),
            status: STEP_STATUS_EXECUTING.to_string(),
            failure_reason: None,
            start_time: now,
            end_time: None,
            attempt_count: 1,
            callback: Some(CallbackGrant {
                token: token.clone(),
                // What the STEP declared, which is what `CallbackStepMetadata$SqsQueueUrl`
                // reports: the queue the definition named.
                queue_url: declared_url.to_string(),
                output_parameters: Vec::new(),
                settled_by: None,
            }),
        };
        match callback_queue_address(region, declared_url) {
            Some(address) => dispatch.deliveries.push(CallbackDelivery {
                step_name: step.name.clone(),
                queue_url: address,
                message: callback_message(&token, step.arguments.as_ref()),
            }),
            None => run.settle(
                now,
                STEP_STATUS_FAILED,
                Some(callback_step_names_no_queue()),
                None,
            ),
        }
        dispatch.runs.push(run);
    }
    dispatch
}

/// Mint the token a callback step is dispatched with: the first
/// [`CALLBACK_TOKEN_LENGTH`] digits of [`mint_opaque_resource_id`]'s hex id, which is the
/// modelled `CallbackToken` form (ten characters of `[a-zA-Z0-9]`).
///
/// The token is the server's to generate — a worker learns it from the message, and nothing
/// in the request names it — so it is derived from nothing the caller sent. It is an
/// IDENTIFIER inside this authenticated store rather than a secret, exactly as the id it
/// comes from is: the model fixes its length at ten characters, which is a name for a
/// waiting step and not something that could carry a credential's worth of bits, and what a
/// token resolves to is one step of one run in this process.
#[must_use]
pub fn mint_callback_token() -> String {
    mint_opaque_resource_id()
        .chars()
        .take(CALLBACK_TOKEN_LENGTH)
        .collect()
}

/// The body of the message a dispatched callback step sends: the token the step was
/// dispatched with and the step's own `Arguments`, under the two members the Amazon SQS
/// message SageMaker Pipelines sends carries.
///
/// A step declaring no arguments still carries the member, as the empty object: the worker
/// reads one shape whatever the step declared.
#[must_use]
pub fn callback_message(token: &str, arguments: Option<&::serde_json::Value>) -> String {
    let arguments = arguments
        .cloned()
        .unwrap_or_else(|| ::serde_json::Value::Object(::serde_json::Map::new()));
    ::serde_json::json!({
        CALLBACK_MESSAGE_TOKEN: token,
        CALLBACK_MESSAGE_ARGUMENTS: arguments,
    })
    .to_string()
}

/// The body of the ADDITIONAL message a stop sends to a waiting callback step's queue: the
/// same token and arguments the dispatch sent, plus the `Status` field set to
/// [`CALLBACK_STATUS_STOPPING`].
///
/// `StopPipelineExecution`'s own doc: "A pipeline execution won't stop while a callback step
/// is running. When you call `StopPipelineExecution` on a pipeline execution with a running
/// callback step, SageMaker Pipelines sends an additional Amazon SQS message to the
/// specified SQS queue." The worker answers it with
/// `SendPipelineExecutionStepSuccess`/`Failure`, and "only when SageMaker Pipelines receives
/// one of these calls will it stop the pipeline execution".
#[must_use]
pub fn callback_stop_message(token: &str, arguments: Option<&::serde_json::Value>) -> String {
    let arguments = arguments
        .cloned()
        .unwrap_or_else(|| ::serde_json::Value::Object(::serde_json::Map::new()));
    ::serde_json::json!({
        CALLBACK_MESSAGE_TOKEN: token,
        CALLBACK_MESSAGE_ARGUMENTS: arguments,
        CALLBACK_MESSAGE_STATUS: CALLBACK_STATUS_STOPPING,
    })
    .to_string()
}

/// Why a dispatched callback step could not hand its work over: the queue it names took no
/// message, so nothing will ever report its token back.
///
/// The address is the one this emulator's own Amazon SQS was asked at
/// ([`callback_queue_address`]) — a Region, this account and a queue name — so the sentence
/// stays inside the modelled `FailureReason` length (`@length 0..1024`) whatever the
/// definition wrote.
#[must_use]
pub fn callback_message_not_delivered(queue_url: &str) -> String {
    format!(
        "The callback step handed its work over to the Amazon SQS queue {queue_url}, which \
         took no message, so no worker can report this step's outcome back."
    )
}

/// Why a callback step was not performed at all: it names no Amazon SQS queue to hand its
/// work over through, so there is nothing to tell and nothing that could report its outcome.
///
/// FIXED text carrying nothing from the definition: what the caller has to supply is the
/// same whether the step declared no `SqsQueueUrl`, one that addresses no queue, or one
/// longer than the member a step execution reports it under admits
/// ([`MAX_CALLBACK_QUEUE_URL_LEN`]).
#[must_use]
pub fn callback_step_names_no_queue() -> String {
    "The callback step names no Amazon SQS queue of this account to hand its work over \
     through, so no worker could be asked to report its outcome."
        .to_string()
}

/// How this service states a run a stop cannot act on: the run and the status it is in,
/// because what a caller acts on is the status that is past stopping.
///
/// `StopPipelineExecution` stops a run that is executing; a run that has already stopped,
/// succeeded or failed has nothing left to stop, which is the conflict the modelled
/// `ConflictException` names.
#[must_use]
pub fn pipeline_execution_not_stoppable(execution_arn: &str, status: &str) -> String {
    format!(
        "Pipeline execution {execution_arn} is {status}, so it is not executing and cannot \
         be stopped."
    )
}

/// How this service states a `CallbackToken` no step of any run is waiting for: a FIXED
/// sentence carrying nothing of the value presented, because the token is a capability — a
/// message quoting one back would hand it to whoever reads the refusal.
///
/// One sentence for the whole class: a token nothing ever minted, one of a run the store no
/// longer holds, and one of a pipeline that was deleted are the same fact to the caller —
/// there is no step waiting for it.
#[must_use]
pub fn callback_token_does_not_exist() -> String {
    "The callback token does not name a pipeline execution step waiting for it.".to_string()
}

/// How this service states a callback step that has already stopped executing: the step,
/// the run it belongs to and the status it settled at, so a worker that reports an outcome
/// twice is told what the step already holds.
#[must_use]
pub fn callback_step_already_settled(step: &str, execution_arn: &str, status: &str) -> String {
    format!(
        "Step {step} of pipeline execution {execution_arn} is already {status}, so its \
         outcome cannot be reported again."
    )
}

/// The reason a callback step carries when the worker that failed it described none: the
/// step failed because it was reported failed, which is all this emulator was told.
#[must_use]
pub fn callback_step_failed_without_a_reason() -> String {
    "The callback step was reported failed with no reason.".to_string()
}

/// Hand one dispatched callback step's message to the queue it names, answering why it
/// could not be delivered — or `None` once Amazon SQS has taken it.
///
/// Amazon SQS is a resource of ANOTHER service, reached the way every emulated service
/// reaches another: the AWS SDK over the emulator's own in-process transport, through the
/// ONE construction of that client this workspace has
/// ([`::service_common::sqs_client::sqs_client`]). So the message is really taken by the
/// queue the caller created, and a queue that takes nothing fails the step rather than
/// leaving it waiting for a worker that was never told.
///
/// The queue's url is NEVER an address this emulator connects to: it travels as the
/// `QueueUrl` member of a `SendMessage` request to this same emulator's own Amazon SQS,
/// which resolves a queue by that url's `/<account>/<name>` path. The endpoint is the shared
/// client's own, built from the Region alone on the in-process transport, so a caller-chosen
/// value in a pipeline definition cannot make the emulator reach anything — a queue url
/// naming no queue of this account simply resolves to nothing and the step is failed.
pub async fn deliver_callback_message(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    delivery: &CallbackDelivery,
) -> Option<String> {
    let answer = ::service_common::sqs_client::sqs_client(ctx, region)
        .send_message()
        .queue_url(&delivery.queue_url)
        .message_body(&delivery.message)
        .send()
        .await;
    match answer {
        Ok(_) => None,
        Err(error) => {
            ::tracing::warn!(
                queue_url = delivery.queue_url,
                step = delivery.step_name,
                error = ?error,
                "a callback step's message was not taken by its queue"
            );
            Some(callback_message_not_delivered(&delivery.queue_url))
        }
    }
}

impl PipelineExecution {
    /// The run of the step `name`, as this execution recorded it when it dispatched that
    /// step — or `None` for a declared step this run performed nothing for.
    #[must_use]
    pub fn step_run(&self, name: &str) -> Option<&PipelineStepRun> {
        self.steps.iter().find(|step| step.name == name)
    }
}

impl ComAmazonawsSagemakerSagemakerState {
    /// The execution holding the step `token` was minted for, and that step's position in
    /// its run — whatever status the step is in, so a token presented twice is told the
    /// step is no longer waiting rather than that the token names nothing.
    ///
    /// A token is not a record of its own: it is recorded on the step it dispatched, so
    /// this is the one resolution of one, and an execution the store no longer holds takes
    /// its tokens with it.
    pub fn step_with_callback_token(
        &mut self,
        token: &str,
    ) -> Option<(&mut PipelineExecution, usize)> {
        self.pipeline_executions.values_mut().find_map(|execution| {
            let index = execution.steps.iter().position(|step| {
                step.callback
                    .as_ref()
                    .is_some_and(|grant| grant.token == token)
            })?;
            Some((execution, index))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A declaration of one step of `step_type`, naming `queue` where it carries one.
    fn declaration(name: &str, step_type: &str, queue: Option<&str>) -> PipelineStepDeclaration {
        PipelineStepDeclaration {
            name: name.to_string(),
            display_name: None,
            description: None,
            step_type: step_type.to_string(),
            sqs_queue_url: queue.map(str::to_string),
            arguments: Some(::serde_json::json!({ "Marker": name })),
        }
    }

    /// RULE: a callback step whose declared queue addresses a queue of this account is
    /// dispatched — a token minted, a run recorded `Executing`, and a message owed to the
    /// address this emulator's own Amazon SQS takes it at; one that names no such queue is
    /// recorded FAILED at once; and every other kind of step is left undispatched and
    /// counted.
    #[test]
    fn only_a_callback_step_addressing_a_queue_is_dispatched() {
        let account = ::engine::emulator_account_id();
        let queue = |name: &str| format!("https://sqs.us-east-1.amazonaws.com/{account}/{name}");
        // Rows differ along the step's kind and along the form of the queue it names: an
        // addressable url, none at all, an empty one, one past the length the member a step
        // reports it under admits, and one of another account's.
        let too_long = format!("{}{}", queue("long"), "x".repeat(MAX_CALLBACK_QUEUE_URL_LEN));
        let declared = vec![
            declaration("Callback", PIPELINE_CALLBACK_STEP_TYPE, Some(&queue("one"))),
            declaration("Training", "Training", None),
            declaration("Processing", "Processing", Some(&queue("two"))),
            declaration("CallbackNoQueue", PIPELINE_CALLBACK_STEP_TYPE, None),
            declaration("CallbackEmptyQueue", PIPELINE_CALLBACK_STEP_TYPE, Some("")),
            declaration("CallbackLongQueue", PIPELINE_CALLBACK_STEP_TYPE, Some(&too_long)),
            declaration("Second", PIPELINE_CALLBACK_STEP_TYPE, Some(&queue("three"))),
        ];
        let now = SystemTime::now();
        let dispatch = dispatch_pipeline_steps(&declared, "us-east-1", now);

        let recorded: Vec<(&str, &str)> = dispatch
            .runs
            .iter()
            .map(|run| (run.name.as_str(), run.status.as_str()))
            .collect();
        assert_eq!(
            recorded,
            vec![
                ("Callback", STEP_STATUS_EXECUTING),
                ("CallbackNoQueue", STEP_STATUS_FAILED),
                ("CallbackEmptyQueue", STEP_STATUS_FAILED),
                ("CallbackLongQueue", STEP_STATUS_FAILED),
                ("Second", STEP_STATUS_EXECUTING),
            ],
            "case \"the recorded steps\": a callback step addressing a queue executes and \
             one that names none fails at once, got {dispatch:?}"
        );
        assert_eq!(
            dispatch.undispatched, 2,
            "case \"the undispatched steps\": a step of another kind is counted, got \
             {dispatch:?}"
        );
        let queues: Vec<&str> = dispatch
            .deliveries
            .iter()
            .map(|delivery| delivery.queue_url.as_str())
            .collect();
        assert_eq!(
            queues,
            vec![queue("one").as_str(), queue("three").as_str()],
            "case \"the messages owed\": one per dispatched step, at the address this \
             emulator's own Amazon SQS takes it, got {dispatch:?}"
        );
        for run in dispatch
            .runs
            .iter()
            .filter(|run| run.status == STEP_STATUS_EXECUTING)
        {
            let grant = run
                .callback
                .as_ref()
                .unwrap_or_else(|| panic!("case {:?}: a dispatched callback step holds its grant", run.name));
            assert_eq!(
                (run.status.as_str(), run.start_time, run.end_time, run.attempt_count),
                (STEP_STATUS_EXECUTING, now, None, 1),
                "case {:?}: a dispatched step is executing from the instant it was \
                 dispatched, got {run:?}",
                run.name
            );
            assert_eq!(
                grant.token.len(),
                CALLBACK_TOKEN_LENGTH,
                "case {:?}: the token is the modelled length, got {:?}",
                run.name,
                grant.token
            );
            assert!(
                grant.token.chars().all(char::is_alphanumeric),
                "case {:?}: the token is alphanumeric, got {:?}",
                run.name,
                grant.token
            );
        }
        let tokens: ::std::collections::BTreeSet<&str> = dispatch
            .runs
            .iter()
            .filter_map(|run| run.callback.as_ref().map(|grant| grant.token.as_str()))
            .collect();
        assert_eq!(
            tokens.len(),
            dispatch.runs.len(),
            "case \"every recorded step\": each is dispatched with its own token, got \
             {dispatch:?}"
        );
    }

    /// RULE: the address a delivery presents is built from the Region and the
    /// `/<account>/<name>` path the declared url names — the scheme and the authority a
    /// definition wrote are discarded — and a value naming no queue addresses nothing.
    #[test]
    fn a_callback_queues_address_is_built_from_the_queue_the_url_names() {
        let account = ::engine::emulator_account_id();
        // Rows differ along the form the declared url arrives in: the one Amazon SQS hands
        // a client, another Region's authority, a host that is no endpoint of AWS's at all,
        // another account's queue, one carrying no account segment, one carrying no queue
        // name, and one past the reportable length.
        let rows: Vec<(&str, String, Option<String>)> = vec![
            (
                "the url Amazon SQS handed the client",
                format!("https://sqs.us-east-1.amazonaws.com/{account}/orders"),
                Some(format!("https://sqs.eu-west-1.amazonaws.com/{account}/orders")),
            ),
            (
                "an authority of another Region",
                format!("https://sqs.ap-south-1.amazonaws.com/{account}/orders"),
                Some(format!("https://sqs.eu-west-1.amazonaws.com/{account}/orders")),
            ),
            (
                "a host that is no endpoint of AWS's",
                format!("http://a.host.of.no.service.test/{account}/orders"),
                Some(format!("https://sqs.eu-west-1.amazonaws.com/{account}/orders")),
            ),
            (
                "another account's queue",
                "https://sqs.us-east-1.amazonaws.com/111111111111/orders".to_string(),
                Some("https://sqs.eu-west-1.amazonaws.com/111111111111/orders".to_string()),
            ),
            (
                "no account segment",
                "https://sqs.us-east-1.amazonaws.com/orders".to_string(),
                Some(format!("https://sqs.eu-west-1.amazonaws.com/{account}/orders")),
            ),
            ("no queue name at all", String::new(), None),
            ("the root path", "/".to_string(), None),
            (
                "past the reportable length",
                format!("https://sqs.us-east-1.amazonaws.com/{account}/{}", "q".repeat(300)),
                None,
            ),
        ];
        for (case, declared, expected) in rows {
            let actual = callback_queue_address("eu-west-1", &declared);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {declared:?} to address {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the message a dispatched callback step sends carries the token it was
    /// dispatched with and the step's own arguments, and the arguments member is present
    /// even for a step that declares none.
    #[test]
    fn a_callback_message_carries_the_token_and_the_steps_arguments() {
        // Rows differ along what the step declares as its arguments.
        let rows: Vec<(&str, Option<::serde_json::Value>, ::serde_json::Value)> = vec![
            (
                "a step with arguments",
                Some(::serde_json::json!({ "input": "s3://bucket/key", "count": 2 })),
                ::serde_json::json!({ "input": "s3://bucket/key", "count": 2 }),
            ),
            (
                "a step with an empty argument object",
                Some(::serde_json::json!({})),
                ::serde_json::json!({}),
            ),
            ("a step with no arguments", None, ::serde_json::json!({})),
        ];
        for (case, arguments, expected) in rows {
            let rendered = callback_message("0123456789", arguments.as_ref());
            let parsed: ::serde_json::Value =
                ::serde_json::from_str(&rendered).expect("the message is JSON");
            assert_eq!(
                parsed.get(CALLBACK_MESSAGE_TOKEN).and_then(|token| token.as_str()),
                Some("0123456789"),
                "case {case:?}: the message carries the token, got {rendered}"
            );
            assert_eq!(
                parsed.get(CALLBACK_MESSAGE_ARGUMENTS),
                Some(&expected),
                "case {case:?}: the message carries the step's arguments, got {rendered}"
            );
        }
    }
}
