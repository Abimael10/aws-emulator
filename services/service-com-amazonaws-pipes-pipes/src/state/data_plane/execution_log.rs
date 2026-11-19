// SPDX-License-Identifier: CC0-1.0

//! The pipe's OWN EXECUTION LOG: the records a RUNNING pipe writes about each execution it
//! takes, to the destination its `LogConfiguration` names.
//!
//! A pipe's `LogConfiguration` is not a value a client reads back: it is an instruction. Its
//! `Level` is "the level of logging detail to include. This applies to all log destinations
//! for the pipe" (`PipeLogConfigurationParameters$Level`, over `LogLevel`'s `OFF | ERROR |
//! INFO | TRACE`) and its `IncludeExecutionData` is "specify `ALL` to include the execution
//! data (specifically, the `payload`, `awsRequest`, and `awsResponse` fields) in the log
//! messages for this pipe" — so a pipe carrying one EMITS log records as it runs, one per
//! stage of each execution, to the destination the configuration names. The rule this module
//! implements: a RUNNING pipe writes, for every execution it takes, the records its
//! configured `Level` admits — [`Level::Off`] none, [`Level::Error`] only the failures,
//! [`Level::Info`] those plus the execution's own start and verdict, [`Level::Trace`] those
//! plus a record per stage the execution reached — each naming the executing pipe
//! ([`RESOURCE_ARN_FIELD`]), the one id shared by every record of that execution
//! ([`EXECUTION_ID_FIELD`]) and the record's own level ([`LOG_LEVEL_FIELD`]), and carrying
//! what the stage was handed in its own [`PAYLOAD_FIELD`] exactly when
//! `IncludeExecutionData` contains [`ALL_EXECUTION_DATA`]. An execution that did not happen
//! leaves no record at all, so a group's records are the executions the pipe took.
//!
//! A record per STAGE is a record per stage the execution HANDS A BATCH TO — the enrichment
//! and the target ([`Execution::stage_started`]). What the SOURCE poll took is the
//! [`EXECUTION_STARTED`] record's own payload rather than a stage record of its own, because
//! that record is what the execution beginning IS; a stage that failed the execution is named
//! by its failure record ([`Execution::verdict`]) whichever stage it was.
//!
//! A stage whose INVOCATION the service it calls REFUSED writes one record more, at
//! [`Level::Error`] ([`Execution::invocation_failed`]): the refusal belongs to the called
//! service, so that record names which service refused the pipe's call, with what HTTP status
//! and under which class of error, the resource the call named, the identifier of that answer
//! and what it said ([`invocation_error`]) — the only signal a client has that its enrichment
//! or its target is failing, and the reason `ERROR` is a level an operator alarms on.
//!
//! WHICH destination is written to is the one surface of the three
//! `PipeLogConfigurationParameters` carries that this emulator serves: the CloudWatch Logs
//! group `CloudwatchLogsLogDestination.LogGroupArn` names ([`Destination`]). A pipe
//! configuring only the Amazon S3 or the Amazon Data Firehose destination has nowhere here
//! to write, and writes nothing, rather than having its records diverted to a destination it
//! did not name.
//!
//! The group is the ACCOUNT's, named by a pipe that was pointed at it, so the write goes
//! into the group CloudWatch Logs already holds and creates none
//! ([`::service_common::cloudwatch_logs::delivered_into_held_group_over`], the one
//! vended-logs write in this tree): a destination nothing created loses the records, which is
//! what makes a misconfigured destination observable. The write is made AS THE PIPE'S
//! EXECUTION ROLE like every other data-plane call ([`super::LiveRecord::refusal`]), and it is
//! authorized on the resource the calls it makes ADDRESS — the log STREAM the records go into
//! ([`Destination::stream_resource`]), which is what CloudWatch Logs judges a call naming a
//! stream against, rather than whichever form the destination's `LogGroupArn` was written in.
//! So a role granted those two actions over the group's streams may write however the
//! destination was spelled, and a role granted neither writes nothing; the execution itself is
//! unaffected either way — logging a pass is never what decides whether its records were
//! moved.

use ::serde_json::{json, Value};

use super::called_service::Refusal;
use super::{Batch, FailedAttempt, LiveRecord, RunningPipe, Stage};
use crate::ops::types::PipeLogConfiguration;

/// The IAM actions the write is authorized under: the API calls it makes, in CloudWatch
/// Logs' own namespace — the stream the records go into, and the records themselves.
const CREATE_LOG_STREAM_ACTION: &str = "logs:CreateLogStream";
/// See [`CREATE_LOG_STREAM_ACTION`].
const PUT_LOG_EVENTS_ACTION: &str = "logs:PutLogEvents";

/// The one value `IncludeExecutionData` carries: "specify `ALL` to include the execution
/// data" (`IncludeExecutionDataOption`'s single member).
const ALL_EXECUTION_DATA: &str = "ALL";

/// The resource type a CloudWatch Logs group's ARN names
/// (`arn:<partition>:logs:<region>:<account>:log-group:<name>`).
const LOG_GROUP_RESOURCE_TYPE: &str = "log-group";

/// What follows a GROUP's ARN in the ARN of one STREAM of that group
/// (`…:log-group:<name>:log-stream:<stream>`). See [`Destination::stream_resource`].
const STREAM_RESOURCE_QUALIFIER: &str = "log-stream:";

/// The fields every record of an execution carries: the pipe that ran, the instant the
/// record was written, the execution every record of it shares, the record's own level and
/// what kind of record it is.
const RESOURCE_ARN_FIELD: &str = "resourceArn";
/// See [`RESOURCE_ARN_FIELD`].
const TIMESTAMP_FIELD: &str = "timestamp";
/// See [`RESOURCE_ARN_FIELD`].
const EXECUTION_ID_FIELD: &str = "executionId";
/// See [`RESOURCE_ARN_FIELD`].
const LOG_LEVEL_FIELD: &str = "logLevel";
/// See [`RESOURCE_ARN_FIELD`].
const MESSAGE_TYPE_FIELD: &str = "messageType";

/// The field the EXECUTION DATA travels in: "the `payload`, `awsRequest`, and `awsResponse`
/// fields", of which this is the one a pipe's own stages produce — the payload the stage the
/// record is about was handed, or answered, as the text it was handed over as. Written
/// exactly when `IncludeExecutionData` contains [`ALL_EXECUTION_DATA`].
const PAYLOAD_FIELD: &str = "payload";

/// The stage of the pipe a stage record is about, as [`Stage::reported_stage`] names it —
/// the same naming the record of a discarded batch uses ([`super::dead_letter`]), so one
/// stage is never called two things.
const STAGE_FIELD: &str = "stage";

/// What a record about a FAILURE says went wrong.
const ERROR_FIELD: &str = "error";

/// The members the [`ERROR_FIELD`] of a record about a REFUSED INVOCATION carries: which
/// service refused, with what HTTP status and under which class, the resource the refused
/// call named, the identifier of that answer, and what it said.
///
/// They are the facts of the call the stage made ([`super::called_service`]), so a client
/// reading the record is told which service failed its pipe rather than only that one did.
const AWS_SERVICE_FIELD: &str = "awsService";
/// See [`AWS_SERVICE_FIELD`].
const HTTP_STATUS_CODE_FIELD: &str = "httpStatusCode";
/// See [`AWS_SERVICE_FIELD`].
const EXCEPTION_TYPE_FIELD: &str = "exceptionType";
/// See [`AWS_SERVICE_FIELD`].
const REQUEST_ID_FIELD: &str = "requestId";
/// See [`AWS_SERVICE_FIELD`].
const MESSAGE_FIELD: &str = "message";

/// The kinds of record an execution writes.
const EXECUTION_STARTED: &str = "ExecutionStarted";
/// See [`EXECUTION_STARTED`].
const EXECUTION_SUCCEEDED: &str = "ExecutionSucceeded";
/// See [`EXECUTION_STARTED`].
const EXECUTION_FAILED: &str = "ExecutionFailed";
/// See [`EXECUTION_STARTED`].
const EXECUTION_STAGE_STARTED: &str = "ExecutionStageStarted";
/// See [`EXECUTION_STARTED`].
const EXECUTION_STAGE_SUCCEEDED: &str = "ExecutionStageSucceeded";
/// See [`EXECUTION_STARTED`].
const EXECUTION_STAGE_FAILED: &str = "ExecutionStageFailed";

/// The kind of record the INVOCATION a stage made being refused is reported by, one per
/// stage that makes one: the enrichment's and the target's
/// ([`Execution::invocation_failed`]).
const ENRICHMENT_INVOCATION_FAILED: &str = "EnrichmentInvocationFailed";
/// See [`ENRICHMENT_INVOCATION_FAILED`].
const TARGET_INVOCATION_FAILED: &str = "TargetInvocationFailed";

/// How a record about a refused invocation OPENS its message, per stage: the stage whose
/// invocation failed, then the service that refused it ([`invocation_message`]).
const ENRICHMENT_INVOCATION_OPENING: &str = "Enrichment invocation failed";
/// See [`ENRICHMENT_INVOCATION_OPENING`].
const TARGET_INVOCATION_OPENING: &str = "Target invocation failed";

/// What joins that opening to the service that refused and the sentence it worded.
const WITH_ERROR_FROM: &str = "with error from";

/// The most records ONE execution writes.
///
/// This is the EMULATOR's bound, not an AWS number: the records below are one per stage of
/// one execution, which is already a handful, so this only stops a configuration nobody
/// expects from writing without bound.
const MAX_RECORDS_PER_EXECUTION: usize = 32;

/// The most bytes of execution data ONE record carries.
///
/// A batch holds up to `super::MAX_RECORDS_PER_BATCH` records and each of their payloads is
/// bounded only by what the source took, so what a stage was handed is not bounded by
/// anything a record could carry: CloudWatch Logs holds no log event past 1 MB
/// (`PutLogEvents`), and a batch of events past this is reported WITHOUT its payload rather
/// than as a put the destination refuses whole — the execution is still logged, and the
/// records that name it still read back.
const MAX_LOGGED_PAYLOAD_BYTES: usize = 32 * 1024;

/// "The level of logging detail to include" (`PipeLogConfigurationParameters$Level`), as the
/// four values `LogLevel` carries.
///
/// The order is the detail each admits: a record is written when its own level is no finer
/// than the pipe's ([`Level::admits`]), so `ERROR` carries only the failures, `INFO` those
/// plus the execution's start and verdict, and `TRACE` those plus the per-stage records.
/// `OFF` admits nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Level {
    /// No record at all.
    #[default]
    Off,
    /// Only what failed.
    Error,
    /// The execution's own start and verdict, and the failures.
    Info,
    /// Every record, including one per stage the execution reached.
    Trace,
}

impl Level {
    /// The level `value` names, or [`Level::Off`] for a value naming none — a pipe whose
    /// configuration this emulator cannot read as a level logs nothing rather than logging at
    /// a level it guessed.
    fn of(value: &str) -> Self {
        match value {
            "ERROR" => Self::Error,
            "INFO" => Self::Info,
            "TRACE" => Self::Trace,
            _ => Self::Off,
        }
    }

    /// This level as a record reports itself, or `None` for [`Level::Off`], which no record
    /// carries.
    fn reported(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Error => Some("ERROR"),
            Self::Info => Some("INFO"),
            Self::Trace => Some("TRACE"),
        }
    }

    /// Whether a pipe configured at this level writes a record of its own level `record`.
    fn admits(self, record: Self) -> bool {
        self != Self::Off && record != Self::Off && record <= self
    }
}

/// The CloudWatch Logs group a pipe's records go into: the group
/// `CloudwatchLogsLogDestination.LogGroupArn` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Destination {
    /// The GROUP's own ARN, carrying nothing of what is under it whichever form the
    /// configuration spelled: what the resource the write is authorized on is built from
    /// ([`Destination::stream_resource`]).
    group_arn: String,
    /// The group's name, which is what the write addresses.
    group: String,
    /// The Region the ARN names — the records go where the ARN says they do.
    region: String,
}

impl Destination {
    /// The ARN of the log STREAM `stream` of this group: the resource a call that names that
    /// stream is authorized on.
    ///
    /// Both calls the write makes NAME the stream they write into, and a call that names a
    /// stream is authorized against that stream's own ARN —
    /// `<the group's ARN>:log-stream:<stream>` — so that is the resource asked about, rather
    /// than whichever of the group ARN's two published forms the destination happened to
    /// carry. It is what makes a grant written over the group's streams the grant that lets a
    /// pipe write its execution log either way round.
    fn stream_resource(&self, stream: &str) -> String {
        format!("{}:{STREAM_RESOURCE_QUALIFIER}{stream}", self.group_arn)
    }
}

/// What a pipe's `LogConfiguration` asks this data plane to WRITE: at which level, with the
/// execution data or without it, and into which group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Logging {
    level: Level,
    /// Whether `IncludeExecutionData` contains [`ALL_EXECUTION_DATA`] — "by default,
    /// execution data is not included".
    execution_data: bool,
    /// The CloudWatch Logs destination, when the configuration names one this emulator
    /// writes to.
    destination: Option<Destination>,
}

impl Logging {
    /// Whether a pipe configured this way writes anything at all: a level above `OFF` AND a
    /// destination to write to.
    fn writes(&self) -> bool {
        self.level != Level::Off && self.destination.is_some()
    }

    /// Whether a record of level `level` is one this configuration writes: the level admits
    /// it AND there is a destination for it to go to.
    fn admits_record(&self, level: Level) -> bool {
        self.destination.is_some() && self.level.admits(level)
    }
}

/// What a pipe carrying the stored `LogConfiguration` `configuration` asks to be logged.
///
/// A pipe carrying none logs nothing, and so does one whose configuration names no
/// destination this emulator writes to — its records are not diverted somewhere it did not
/// name.
pub(super) fn resolve(configuration: Option<&PipeLogConfiguration>) -> Logging {
    let Some(configuration) = configuration else {
        return Logging::default();
    };
    Logging {
        level: configuration
            .level
            .as_deref()
            .map(Level::of)
            .unwrap_or_default(),
        execution_data: includes_execution_data(configuration),
        destination: cloudwatch_destination(configuration),
    }
}

/// Whether `configuration` asks for the execution data: "specify `ALL` to include the
/// execution data … By default, execution data is not included".
fn includes_execution_data(configuration: &PipeLogConfiguration) -> bool {
    configuration
        .include_execution_data
        .as_ref()
        .is_some_and(|options| options.iter().any(|option| option == ALL_EXECUTION_DATA))
}

/// The CloudWatch Logs group `configuration` names, or `None` when it names none this
/// emulator writes into.
///
/// The ARN is read through the one reading of a log group's own ARN in this tree
/// ([`::service_common::cloudwatch_logs::log_group_of_arn`]), which admits both published
/// forms — the group, and the group with everything under it — so a destination a client
/// read off `DescribeLogGroups` and one it wrote by hand name the same group.
fn cloudwatch_destination(configuration: &PipeLogConfiguration) -> Option<Destination> {
    let arn = configuration
        .cloudwatch_logs_log_destination
        .as_ref()?
        .log_group_arn
        .as_deref()?;
    let group = ::service_common::cloudwatch_logs::log_group_of_arn(arn)?;
    let parsed = ::service_common::arn::parse(arn)?;
    Some(Destination {
        // The group's OWN ARN, rebuilt from the fields the configured one carries, so that
        // both published forms of a destination resolve to the one ARN of the one group.
        group_arn: format!(
            "arn:{}:{}:{}:{}:{LOG_GROUP_RESOURCE_TYPE}:{group}",
            parsed.partition, parsed.vendor, parsed.region, parsed.account
        ),
        group: group.to_string(),
        region: parsed.region.to_string(),
    })
}

/// ONE execution of a pipe, as its log records see it: the id every record of it shares and
/// the records written so far.
///
/// An execution is the pass that moved one batch, so the id below is minted once per batch
/// the poller takes and every record the stages produce carries it — which is what lets a
/// reader ask for the records of one execution and get that execution's only.
pub(super) struct Execution<'a> {
    /// The pipe as the batch was DECIDED under, which is where the configuration being
    /// logged under is read from.
    run: &'a RunningPipe,
    /// The execution's own id, or empty for a pipe that writes nothing — nothing is minted
    /// for an execution no record is written about.
    id: String,
    /// The records this execution has produced, in the order the stages produced them.
    records: Vec<Value>,
}

impl ::std::fmt::Debug for Execution<'_> {
    /// The execution and what it has to say, which is all of this that is not the pipe it
    /// belongs to.
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("Execution")
            .field("pipe", &self.run.pipe_arn)
            .field("id", &self.id)
            .field("records", &self.records.len())
            .finish()
    }
}

impl<'a> Execution<'a> {
    /// Begin the execution of the batch `run` was decided under: one id for every record of
    /// it, minted only for a pipe that writes records at all.
    pub(super) fn start(run: &'a RunningPipe) -> Self {
        let id = if run.logging.writes() {
            ::uuid::Uuid::new_v4().to_string()
        } else {
            String::new()
        };
        Self {
            run,
            id,
            records: Vec::new(),
        }
    }

    /// The execution TOOK `batch` from its source and entered the pipe with it.
    pub(super) fn started(&mut self, batch: &[Value]) {
        self.record(Level::Info, EXECUTION_STARTED, None, Some(batch), None);
    }

    /// `stage` was handed `payload`.
    pub(super) fn stage_started(&mut self, stage: Stage, payload: &[Value]) {
        self.record(
            Level::Trace,
            EXECUTION_STAGE_STARTED,
            Some(stage),
            Some(payload),
            None,
        );
    }

    /// `stage` took what it was handed and answered `answered` — the events an enrichment
    /// handed back, and nothing at all for a stage that answers nothing about them.
    pub(super) fn stage_succeeded(&mut self, stage: Stage, answered: Option<&[Value]>) {
        self.record(
            Level::Trace,
            EXECUTION_STAGE_SUCCEEDED,
            Some(stage),
            answered,
            None,
        );
    }

    /// The INVOCATION `stage` made was REFUSED by the service it was made to: the record
    /// that names which service refused it and what it said.
    ///
    /// One record per refused invocation, carrying what that stage was handed — so a client
    /// alarming on the pipe's `ERROR` records is told which service failed its pipe, with
    /// what status and over which record, and never only that something failed. A stage the
    /// pipe could not even make its call for (a role that may not make it, a payload that
    /// could not be built) has no answer from any service to report and leaves this record
    /// out; its failure is the execution's own ([`Execution::verdict`]) as before.
    pub(super) fn invocation_failed(
        &mut self,
        stage: Stage,
        payload: &[Value],
        refusal: &Refusal,
    ) {
        let Some((message_type, opening)) = invocation_failure(stage) else {
            return;
        };
        self.record(
            Level::Error,
            message_type,
            None,
            Some(payload),
            Some(invocation_error(opening, refusal)),
        );
    }

    /// What the pass the execution's stages came to is: its verdict record, and the record of
    /// the stage that failed it when one did.
    ///
    /// Read from the OUTCOME of the stages rather than written by each of them, so the
    /// verdict a client reads and the one the poller acts on are the same answer.
    pub(super) fn verdict(&mut self, outcome: &Result<Batch, FailedAttempt>) {
        match outcome {
            Ok(Batch::Moved { .. }) => {
                self.record(Level::Info, EXECUTION_SUCCEEDED, None, None, None);
            }
            Ok(Batch::NotProcessed { detail, .. }) => {
                self.failed(Stage::Target, detail);
            }
            Ok(Batch::Failed { stage, detail, .. }) => {
                self.failed(*stage, detail);
            }
            // Nothing was asked of a stage: the pipe's record stopped asking for this
            // configuration while the batch was in flight, so there is no verdict to report
            // about what its stages did.
            Ok(Batch::Idle | Batch::Superseded | Batch::Discarded { .. }) => {}
            Err(failure) => self.failed(failure.stage, &failure.detail),
        }
    }

    /// The execution FAILED at `stage`, for `detail`: the stage's own failure record and the
    /// execution's.
    fn failed(&mut self, stage: Stage, detail: &str) {
        self.record(
            Level::Error,
            EXECUTION_STAGE_FAILED,
            Some(stage),
            None,
            Some(json!(detail)),
        );
        self.record(
            Level::Error,
            EXECUTION_FAILED,
            None,
            None,
            Some(json!(detail)),
        );
    }

    /// Hold one record of this execution, if the pipe's configured level admits a record of
    /// level `level`.
    ///
    /// `payload` is what the stage was handed or answered, written only when the pipe asks
    /// for the execution data and only when it is small enough for one log event to carry
    /// ([`MAX_LOGGED_PAYLOAD_BYTES`]).
    fn record(
        &mut self,
        level: Level,
        message_type: &str,
        stage: Option<Stage>,
        payload: Option<&[Value]>,
        error: Option<Value>,
    ) {
        if !self.run.logging.admits_record(level) || self.records.len() >= MAX_RECORDS_PER_EXECUTION
        {
            return;
        }
        let Some(reported_level) = level.reported() else {
            return;
        };
        let mut record = json!({
            RESOURCE_ARN_FIELD: self.run.pipe_arn,
            TIMESTAMP_FIELD: written_at(),
            EXECUTION_ID_FIELD: self.id,
            LOG_LEVEL_FIELD: reported_level,
            MESSAGE_TYPE_FIELD: message_type,
        });
        if let Some(stage) = stage {
            record[STAGE_FIELD] = json!(stage.reported_stage());
        }
        if let Some(detail) = error {
            record[ERROR_FIELD] = detail;
        }
        if let Some(payload) = payload.filter(|_| self.run.logging.execution_data) {
            if let Some(data) = logged_payload(payload) {
                record[PAYLOAD_FIELD] = data;
            }
        }
        self.records.push(record);
    }

    /// Write the records this execution produced to the pipe's configured destination.
    ///
    /// Best effort, as every vended-logs delivery is: a role that may not write them, a group
    /// the account does not hold and a put CloudWatch Logs refuses each lose the records and
    /// are reported in tracing — the execution itself has already happened, and whether its
    /// log was written is never what decides what its stages did.
    ///
    /// Call it with NO store guard held: it awaits.
    pub(super) async fn write(
        &self,
        callback: &smithy_server_runtime::EmbeddedEmulator,
        record: &LiveRecord<'_>,
    ) {
        if self.records.is_empty() {
            return;
        }
        let Some(destination) = self.run.logging.destination.as_ref() else {
            return;
        };
        let stream = stream_of(record.pipe);
        // Both calls name the stream the records go into, so both are authorized on that
        // stream's own ARN ([`Destination::stream_resource`]).
        let resource = destination.stream_resource(&stream);
        for action in [CREATE_LOG_STREAM_ACTION, PUT_LOG_EVENTS_ACTION] {
            if let Some(detail) = record.refusal(action, &resource) {
                ::tracing::debug!(
                    pipe = %record.pipe,
                    log_group = %destination.group,
                    detail = %detail,
                    "a pipe's execution role may not write its execution log, so this \
                     execution's records are not delivered"
                );
                return;
            }
        }
        let messages: Vec<String> = self
            .records
            .iter()
            .map(::std::string::ToString::to_string)
            .collect();
        if !::service_common::cloudwatch_logs::delivered_into_held_group_over(
            callback,
            &destination.region,
            &destination.group,
            &stream,
            &messages,
        )
        .await
        {
            ::tracing::warn!(
                pipe = %record.pipe,
                log_group = %destination.group,
                records = messages.len(),
                "a pipe's execution log could not be written to its configured destination"
            );
        }
    }
}

/// How a refused invocation of `stage` is reported: the kind of record it is, and how that
/// record's message opens.
///
/// `None` for a stage that makes no invocation of its own — the source POLL is the execution
/// beginning rather than a call the pipe hands a batch to, so a poll that failed is the
/// execution's own failure and nothing narrower.
fn invocation_failure(stage: Stage) -> Option<(&'static str, &'static str)> {
    match stage {
        Stage::Source => None,
        Stage::Enrichment => Some((
            ENRICHMENT_INVOCATION_FAILED,
            ENRICHMENT_INVOCATION_OPENING,
        )),
        Stage::Target => Some((TARGET_INVOCATION_FAILED, TARGET_INVOCATION_OPENING)),
    }
}

/// The [`ERROR_FIELD`] of a record about a refused invocation: the facts of the call the
/// stage made, and the one sentence they come to.
///
/// The sentence is the stage's own `opening`, the service that refused, that service's own
/// words for the refusal, and the clause naming the call it came from — the workspace's one
/// rendering of a relayed call ([`::service_common::relayed_refusal`]), so the request id
/// the sentence states and the one the field beside it carries are the one identifier of the
/// one answer.
fn invocation_error(opening: &str, refusal: &Refusal) -> Value {
    let message = format!(
        "{opening} {WITH_ERROR_FROM} {}: {}{}",
        refusal.service_named(),
        refusal.reason(),
        ::service_common::relayed_refusal::relay_envelope_of(
            ::service_common::relayed_refusal::RelayedCall::once(
                refusal.service_sdk(),
                refusal.status(),
            ),
            refusal.request_id(),
        )
    );
    json!({
        AWS_SERVICE_FIELD: refusal.namespace(),
        HTTP_STATUS_CODE_FIELD: refusal.status(),
        EXCEPTION_TYPE_FIELD: refusal.exception_type(),
        RESOURCE_ARN_FIELD: refusal.resource(),
        REQUEST_ID_FIELD: refusal.request_id(),
        MESSAGE_FIELD: message,
    })
}

/// The log stream one pipe's records go into, inside the group its configuration names.
///
/// One stream per pipe, named by the pipe: this is the EMULATOR's choice, bounded so that a
/// pipe running for as long as it likes occupies one stream of its group rather than one per
/// execution (`MAX_STREAMS_PER_GROUP` of the service that holds them). What tells the records
/// of one execution from another's is the [`EXECUTION_ID_FIELD`] every record carries, which
/// is what a reader of the group filters on.
fn stream_of(pipe: &str) -> String {
    pipe.to_string()
}

/// The instant a record is written, as the text it reports: the one rendering of an instant
/// a pipe's own records use ([`super::dead_letter::instant_text`]).
fn written_at() -> String {
    super::dead_letter::instant_text(&::aws_smithy_types::DateTime::from(
        ::std::time::SystemTime::now(),
    ))
    .unwrap_or_default()
}

/// `payload` as a record carries it: the TEXT of what the stage was handed — the one event
/// when it was handed exactly one, the array itself otherwise — or `None` when that text is
/// longer than one log event may carry.
///
/// The execution data travels as TEXT rather than as a nested document: what a pipe hands
/// from one stage to the next is a payload, and the record reports the payload it handed
/// over, so a reader looking for the source record's own data finds it in this field rather
/// than having to walk a structure the record invented around it. A stage handed one event
/// is the common case of a batching pipe (`BatchSize: 1`), and the data of such an execution
/// IS that event.
fn logged_payload(payload: &[Value]) -> Option<Value> {
    let text = match payload {
        [] => return None,
        [one] => one.to_string(),
        many => Value::Array(many.to_vec()).to_string(),
    };
    (text.len() <= MAX_LOGGED_PAYLOAD_BYTES).then_some(Value::String(text))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::kinesis_run;
    use super::{
        logged_payload, resolve, Level, Logging, MAX_LOGGED_PAYLOAD_BYTES, PAYLOAD_FIELD,
    };
    use crate::ops::types::{CloudwatchLogsLogDestination, PipeLogConfiguration};

    /// RULE: the resource the execution-log write is authorized on is the ARN of the STREAM
    /// the records go into, inside the group the destination resolves to, whichever published
    /// form the configured `LogGroupArn` was written in.
    ///
    /// The rows differ along that form and along the partition, Region and account the
    /// destination names, which is what the rule generalises over.
    #[test]
    fn the_write_is_authorized_on_the_streams_own_arn() {
        let stream = "arn:aws:pipes:eu-west-1:000000000000:pipe/one";
        // (case, the configured destination, the group ARN the resource is built on)
        let rows: [(&str, &str, &str); 4] = [
            (
                "the group on its own",
                "arn:aws:logs:us-east-1:000000000000:log-group:/aws/vendedlogs/pipes/one",
                "arn:aws:logs:us-east-1:000000000000:log-group:/aws/vendedlogs/pipes/one",
            ),
            (
                "the group with everything under it",
                "arn:aws:logs:us-east-1:000000000000:log-group:/aws/vendedlogs/pipes/one:*",
                "arn:aws:logs:us-east-1:000000000000:log-group:/aws/vendedlogs/pipes/one",
            ),
            (
                "another Region and account",
                "arn:aws:logs:ap-south-1:123456789012:log-group:audit",
                "arn:aws:logs:ap-south-1:123456789012:log-group:audit",
            ),
            (
                "another partition",
                "arn:aws-us-gov:logs:us-gov-west-1:000000000000:log-group:audit:*",
                "arn:aws-us-gov:logs:us-gov-west-1:000000000000:log-group:audit",
            ),
        ];
        for (case, configured, group_arn) in rows {
            let logging = resolve(Some(&configuration(Some("TRACE"), Some(configured), true)));
            let destination = logging
                .destination
                .as_ref()
                .unwrap_or_else(|| panic!("case {case:?}: {configured} names no log group"));
            let got = destination.stream_resource(stream);
            let expected = format!("{group_arn}:log-stream:{stream}");
            assert_eq!(
                got, expected,
                "case {case:?}: expected the write to be authorized on {expected:?}, got \
                 {got:?}"
            );
        }
    }

    /// A stored configuration naming `level`, the group `arn` when it names one, and asking
    /// for the execution data when `data` says so.
    fn configuration(level: Option<&str>, arn: Option<&str>, data: bool) -> PipeLogConfiguration {
        PipeLogConfiguration {
            cloudwatch_logs_log_destination: arn.map(|arn| CloudwatchLogsLogDestination {
                log_group_arn: Some(arn.to_string()),
            }),
            level: level.map(ToString::to_string),
            include_execution_data: data.then(|| vec!["ALL".to_string()]),
            ..PipeLogConfiguration::default()
        }
    }

    /// RULE: a pipe writes the records its configured `Level` admits — `OFF` none, `ERROR`
    /// only the failures, `INFO` those plus the execution's own records, `TRACE` those plus
    /// the per-stage ones — and writes nothing at all without a destination to write to.
    ///
    /// The rows differ along the configured level and the record's own level, which is what
    /// the rule generalises over.
    #[test]
    fn a_level_admits_the_records_no_finer_than_itself() {
        // (case, configured, the record's level, whether it is written)
        let rows: [(&str, Level, Level, bool); 12] = [
            ("off admits no failure", Level::Off, Level::Error, false),
            ("off admits no verdict", Level::Off, Level::Info, false),
            ("off admits no stage record", Level::Off, Level::Trace, false),
            ("error admits a failure", Level::Error, Level::Error, true),
            ("error admits no verdict", Level::Error, Level::Info, false),
            (
                "error admits no stage record",
                Level::Error,
                Level::Trace,
                false,
            ),
            ("info admits a failure", Level::Info, Level::Error, true),
            ("info admits a verdict", Level::Info, Level::Info, true),
            (
                "info admits no stage record",
                Level::Info,
                Level::Trace,
                false,
            ),
            ("trace admits a failure", Level::Trace, Level::Error, true),
            ("trace admits a verdict", Level::Trace, Level::Info, true),
            (
                "trace admits a stage record",
                Level::Trace,
                Level::Trace,
                true,
            ),
        ];
        for (case, configured, record, written) in rows {
            let got = configured.admits(record);
            assert_eq!(
                got, written,
                "case {case:?}: expected admits={written} at {configured:?} for {record:?}, \
                 got {got}"
            );
            // And the same level with no destination writes nothing, whatever it admits.
            let logging = Logging {
                level: configured,
                execution_data: true,
                destination: None,
            };
            assert!(
                !logging.admits_record(record),
                "case {case:?}: a pipe with no destination writes nothing, it admitted \
                 {record:?}"
            );
            assert!(
                !logging.writes(),
                "case {case:?}: a pipe with no destination writes nothing at all"
            );
        }
    }

    /// One row of the resolution table: a stored configuration, and what a pipe carrying it
    /// logs under.
    struct Stored {
        case: &'static str,
        /// `Level` as the record carries it.
        level: Option<&'static str>,
        /// `CloudwatchLogsLogDestination.LogGroupArn`, when the record carries one.
        arn: Option<&'static str>,
        /// Whether `IncludeExecutionData` carries `ALL`.
        data: bool,
        /// The level, group and execution-data answer the reading has to come to.
        want_level: Level,
        want_group: Option<&'static str>,
        want_data: bool,
    }

    /// RULE: what a pipe logs is read from its stored configuration — the level it names, the
    /// group its CloudWatch Logs destination's ARN names in both published forms, and the
    /// execution data exactly when `IncludeExecutionData` carries `ALL`.
    ///
    /// The rows differ along the level, the form and kind of the configured destination and
    /// whether the execution data was asked for.
    #[test]
    fn the_stored_configuration_is_what_is_logged_under() {
        let rows = [
            Stored {
                case: "a trace pipe with the execution data",
                level: Some("TRACE"),
                arn: Some(
                    "arn:aws:logs:us-east-1:000000000000:log-group:/aws/vendedlogs/pipes/one",
                ),
                data: true,
                want_level: Level::Trace,
                want_group: Some("/aws/vendedlogs/pipes/one"),
                want_data: true,
            },
            Stored {
                case: "a group named with everything under it",
                level: Some("INFO"),
                arn: Some(
                    "arn:aws:logs:eu-west-1:000000000000:log-group:/aws/vendedlogs/pipes/two:*",
                ),
                data: false,
                want_level: Level::Info,
                want_group: Some("/aws/vendedlogs/pipes/two"),
                want_data: false,
            },
            Stored {
                case: "an error pipe",
                level: Some("ERROR"),
                arn: Some("arn:aws:logs:us-east-1:000000000000:log-group:errors-only"),
                data: false,
                want_level: Level::Error,
                want_group: Some("errors-only"),
                want_data: false,
            },
            Stored {
                case: "logging turned off",
                level: Some("OFF"),
                arn: Some("arn:aws:logs:us-east-1:000000000000:log-group:quiet"),
                data: true,
                want_level: Level::Off,
                want_group: Some("quiet"),
                want_data: true,
            },
            Stored {
                case: "a level this emulator cannot read",
                level: Some("VERBOSE"),
                arn: Some("arn:aws:logs:us-east-1:000000000000:log-group:quiet"),
                data: false,
                want_level: Level::Off,
                want_group: Some("quiet"),
                want_data: false,
            },
            Stored {
                case: "no destination at all",
                level: Some("TRACE"),
                arn: None,
                data: true,
                want_level: Level::Trace,
                want_group: None,
                want_data: true,
            },
            Stored {
                case: "a destination that names no log group",
                level: Some("TRACE"),
                arn: Some("arn:aws:s3:::pipe-logs"),
                data: true,
                want_level: Level::Trace,
                want_group: None,
                want_data: true,
            },
        ];
        for row in rows {
            let case = row.case;
            let stored = configuration(row.level, row.arn, row.data);
            let logging = resolve(Some(&stored));
            assert_eq!(
                logging.level, row.want_level,
                "case {case:?}: expected level {:?}, got {logging:?}",
                row.want_level
            );
            assert_eq!(
                logging.destination.as_ref().map(|held| held.group.as_str()),
                row.want_group,
                "case {case:?}: expected the group {:?}, got {logging:?}",
                row.want_group
            );
            assert_eq!(
                logging.execution_data, row.want_data,
                "case {case:?}: expected execution_data={}, got {logging:?}",
                row.want_data
            );
            assert_eq!(
                logging.writes(),
                row.want_level != Level::Off && row.want_group.is_some(),
                "case {case:?}: a pipe writes exactly when it has a level and a destination, \
                 got {logging:?}"
            );
            // And a pipe carrying no configuration at all logs nothing.
            assert_eq!(
                resolve(None),
                Logging::default(),
                "case {case:?}: a pipe with no LogConfiguration logs nothing"
            );
        }
    }

    /// RULE: the execution data a record carries is the TEXT of what the stage was handed —
    /// the one event when it was handed one, the array itself otherwise — and a batch past
    /// what one log event may carry is reported without it rather than not at all.
    ///
    /// The rows differ along how many events the stage was handed and how large they are.
    #[test]
    fn the_execution_data_is_what_the_stage_was_handed() {
        let one = ::serde_json::json!({ "body": "a-marker" });
        let big = ::serde_json::json!({ "body": "x".repeat(MAX_LOGGED_PAYLOAD_BYTES + 1) });
        // (case, the events handed over, what the record carries)
        let rows: [(&str, Vec<::serde_json::Value>, Option<::serde_json::Value>); 4] = [
            ("nothing handed over", vec![], None),
            (
                "one event",
                vec![one.clone()],
                Some(::serde_json::Value::String(one.to_string())),
            ),
            (
                "two events",
                vec![one.clone(), one.clone()],
                Some(::serde_json::Value::String(
                    ::serde_json::json!([one, one]).to_string(),
                )),
            ),
            ("an event past the bound", vec![big], None),
        ];
        for (case, handed, want) in rows {
            let got = logged_payload(&handed);
            assert_eq!(
                got, want,
                "case {case:?}: expected the record's {PAYLOAD_FIELD} to be {want:?}, got \
                 {got:?}"
            );
            // Whatever it holds, a reader of the field finds the data the stage was handed
            // IN it rather than around it.
            if let (Some(::serde_json::Value::String(text)), Some(first)) =
                (got.as_ref(), handed.first())
            {
                assert!(
                    text.contains("a-marker") || first["body"].as_str().is_none(),
                    "case {case:?}: expected the handed event's own data in the field, got \
                     {text:?}"
                );
            }
        }
        // A pipe that did not ask for the execution data carries none of it, whatever the
        // stage was handed — which is what `admits_record` and this reading are used
        // together for.
        let run = kinesis_run("TRIM_HORIZON");
        assert!(
            !run.logging.execution_data,
            "a pipe configuring no logging asks for no execution data, got {:?}",
            run.logging
        );
    }
}
