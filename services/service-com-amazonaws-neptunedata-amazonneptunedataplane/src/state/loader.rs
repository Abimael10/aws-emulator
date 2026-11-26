// SPDX-License-Identifier: CC0-1.0

//! The BULK LOADER: what a load request is read as, what one load leaves behind, and what
//! the job it admitted reports afterwards.
//!
//! A load runs UNDER an IAM role: the loader assumes the role the request names and reads
//! the source with that role's credentials, so a value it cannot resolve a credential for
//! is a load that could never start. `iamRoleArn` is an unconstrained string in the model,
//! so the request boundary admits any text at all and reading the value is this service's
//! own job ([`resolves_credentials`]) — and a value that names no IAM role is refused
//! naming both halves of what failed: the source the load was for, and the role it would
//! have run under ([`unusable_iam_role`]).
//!
//! The ARN itself is read with the crate-wide ARN requirement for an IAM role
//! (`::service_common::arn::IAM_ROLE`), so this service and every other agree on what the
//! ARN of a role is.
//!
//! ## What a load does
//!
//! Everything else here is the LOAD itself. A request names a location in Amazon S3, and
//! the loader reads the objects there out of this emulator's own object store
//! ([`source`]), decodes each one in the format the request named ([`formats`]) and writes
//! the elements it read into this engine's one graph ([`apply_load`]). So every number a
//! job reports is a measurement of that run: a load id is handed back for a load that was
//! really performed, and the graph a later query reads is the graph the load left behind.
//!
//! The run is recorded on the job ([`LoadRun`]) because each of its numbers belongs to ONE
//! load: the status endpoint reports them, the registry keeps them, and nothing recomputes
//! either per read. The job reaches its terminal status when the clock makes it due
//! ([`LOAD_SETTLES_AFTER`]), so a caller that polls straight after starting a load still
//! sees the load in progress, and the operations that report on a job materialise that
//! transition when they read it ([`settle_loader_jobs`](
//! super::ComAmazonawsNeptunedataAmazonneptunedataplaneState::settle_loader_jobs)).
//!
//! Two documented members of a load request have no work to do in this emulator and are
//! recorded rather than acted on: `parallelism` chooses how many THREADS the loader
//! spends on a load, and `queueRequest` lets a request wait behind the loads already
//! running — and this loader reads its source while the request is being served, so no
//! load ever waits behind another and the queue a queued request would join is always
//! empty.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ::serde_json::{json, Value};

use crate::ops::types::{InvalidParameterException, StartLoaderJobInput};

mod formats;
mod source;

pub use formats::*;
pub use source::*;

/// Whether the loader can resolve a credential for `iam_role_arn`.
///
/// A credential is resolved for an IAM ROLE, so the value has to name one: an ARN of this
/// partition's `iam` namespace addressing a `role` resource. A value that is not an ARN at
/// all, an ARN of another service, and an ARN naming another kind of IAM entity each name
/// no role to assume, so no credential can be found for any of them.
#[must_use]
pub fn resolves_credentials(iam_role_arn: &str) -> bool {
    ::service_common::arn::IAM_ROLE.admits(iam_role_arn)
}

/// The `InvalidParameterException` payload for a load the loader could not start because
/// it has no credential for the role the request named.
///
/// Both variable parts are the request's own: the source the load was to read, and the
/// role it was to run under, so the caller can see which load and which role from the
/// message alone.
#[must_use]
pub fn unusable_iam_role(source: &str, iam_role_arn: &str) -> InvalidParameterException {
    InvalidParameterException {
        detailed_message: format!(
            "Failed to start new load from the source {}. Couldn't find the aws credential \
             for iam_role_arn: {}",
            super::echoed(source),
            super::echoed(iam_role_arn)
        ),
        request_id: super::new_request_id(),
    }
}

/// What Neptune answers a request naming a bulk-load job the loader has no record of.
///
/// The one variable part is the id the request named, so a caller can see which load the
/// engine has no record of.
#[must_use]
pub fn load_id_not_found_message(load_id: &str) -> String {
    format!(
        "The load with id = {} does not exist",
        super::echoed(load_id)
    )
}

/// What a CANCEL answers the same id. A cancel acts on a load the loader still owes an
/// outcome, so the id of a job that has already finished names nothing left to stop, and
/// the sentence states both readings of what was wrong with the value.
#[must_use]
pub fn load_not_active_message(load_id: &str) -> String {
    format!("{} or not active", load_id_not_found_message(load_id))
}

/// The `InvalidParameterException` payload for a `loadId` the registry does not hold:
/// there is no load to report on, so the value the request carried is the thing that was
/// wrong with it. The error CLASS a client branches on is written from the shape's own name
/// (see `crate::ops::types::error_message_wire`).
#[must_use]
pub fn invalid_load_id(load_id: &str) -> InvalidParameterException {
    InvalidParameterException {
        detailed_message: load_id_not_found_message(load_id),
        request_id: super::new_request_id(),
    }
}

/// The `InvalidParameterException` payload for a cancel whose `loadId` names no load the
/// loader still owes an outcome: no such job, or one that has already finished.
#[must_use]
pub fn inactive_load_id(load_id: &str) -> InvalidParameterException {
    InvalidParameterException {
        detailed_message: load_not_active_message(load_id),
        request_id: super::new_request_id(),
    }
}

/// `StartLoaderJobOutput$status` for a load request the loader took on — the member is
/// "the HTTP return code indicating the status of the load job", and the loader endpoint
/// states that as the HTTP status line of its answer.
pub const LOADER_JOB_ACCEPTED: &str = "200 OK";

/// `GetLoaderJobStatusOutput$status` for a status request the loader served — "the HTTP
/// response code for the request", stated the same way.
pub const LOADER_JOB_STATUS_SERVED: &str = "200 OK";

/// `CancelLoaderJobOutput$status` for a cancellation the loader accepted. It reports the
/// status of the cancellation request itself, not of the load.
pub const LOADER_JOB_CANCELLATION_ACCEPTED: &str = "200 OK";

/// The one member `StartLoaderJobOutput$payload` carries: the member's documentation says
/// the payload "contains a loadId name-value pair that provides an identifier for the load
/// operation".
pub const LOAD_ID_MEMBER: &str = "loadId";

/// How long after the request that started it a load reaches its terminal status.
///
/// A load Neptune accepts is reported in a non-terminal status first, so this engine owes
/// the same transition: the span is this emulator's own and it is never zero, so a caller
/// reading the job straight after starting it still sees the load in progress.
pub const LOAD_SETTLES_AFTER: Duration = Duration::from_secs(3);

/// The load status of a queued load request.
pub const LOAD_IN_QUEUE: &str = "LOAD_IN_QUEUE";

/// The load status of a load that is reading its source now.
pub const LOAD_IN_PROGRESS: &str = "LOAD_IN_PROGRESS";

/// The load status of a load that read everything its source held.
pub const LOAD_COMPLETED: &str = "LOAD_COMPLETED";

/// The load status of a load that stopped on an error it could not skip.
pub const LOAD_FAILED: &str = "LOAD_FAILED";

/// The load status of a load `CancelLoaderJob` stopped.
pub const LOAD_CANCELLED_BY_USER: &str = "LOAD_CANCELLED_BY_USER";

/// The load status of a load whose source could not be read out of Amazon S3 at all.
pub const LOAD_S3_READ_ERROR: &str = "LOAD_S3_READ_ERROR";

/// The `errorCode` of an error log for data the loader could not read as the format the
/// request named.
pub const PARSING_ERROR_CODE: &str = "PARSING_ERROR";

/// The `errorCode` of an error log for a value that does not read as the type its column
/// declared.
pub const DATATYPE_MISMATCH_ERROR_CODE: &str = "DATATYPE_MISMATCH_ERROR";

/// The `errorCode` of an error log for an edge whose endpoints the load does not hold.
pub const FROM_OR_TO_VERTEX_MISSING_CODE: &str = "FROM_OR_TO_VERTEX_ARE_MISSING";

/// The `errorCode` of an error log for an element the graph would not take.
pub const INSERT_ERROR_CODE: &str = "INSERT_ERROR";

/// The `errorCode` of an error log for a source location the loader could not read.
pub const S3_READ_ERROR_CODE: &str = "S3_READ_ERROR";

/// The `errorCode` of an error log for a load that found no previous load of its source
/// to resume.
pub const RESUME_NOT_POSSIBLE_CODE: &str = "RESUME_NOT_POSSIBLE";

/// How many load jobs the registry keeps.
///
/// "Neptune keeps track of the most recent 1,024 bulk load jobs"
/// (`smithy_query(query="doc",shape="GetLoaderJobStatus")`), so a job past that is
/// forgotten — oldest admission first — rather than letting a stream of loads grow this
/// engine's memory without end.
pub const LOADER_JOB_HISTORY: usize = 1024;

/// `GetLoaderJobStatusInput$page`'s documented default: "The error page number (a positive
/// integer; the default is 1)".
pub const DEFAULT_ERROR_PAGE: i32 = 1;

/// `GetLoaderJobStatusInput$errorsPerPage`'s documented default: "The number of errors
/// returned in each page (a positive integer; the default is 10)".
pub const DEFAULT_ERRORS_PER_PAGE: i32 = 10;

/// The load mode of a request that names none: `mode`'s own documentation says "Allowed
/// values: RESUME, NEW, AUTO. Default value: AUTO".
pub const DEFAULT_MODE: &str = "AUTO";

/// The mode that starts a load "regardless of any previous loads".
pub const MODE_NEW: &str = "NEW";

/// The mode that resumes a previous load of the same source and stops when it finds none.
pub const MODE_RESUME: &str = "RESUME";

/// How far a bulk-load job this engine has admitted has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderJobState {
    /// Admitted as a queued load request, waiting behind the jobs ahead of it.
    Queued,
    /// Loading its source data now.
    Loading,
    /// Finished loading everything the source held.
    Completed,
    /// Stopped on an error it could not skip.
    Failed,
    /// Stopped by `CancelLoaderJob`.
    Cancelled,
}

impl LoaderJobState {
    /// Whether the job is still a queued load request, which is what
    /// `ListLoaderJobsInput$includeQueuedLoads` chooses to see or to exclude.
    pub fn is_queued(self) -> bool {
        matches!(self, Self::Queued)
    }

    /// Whether this is one of the "active loader jobs" `ListLoaderJobs` reports the load
    /// ids of: a job the loader still owes an outcome, queued or loading. A job that has
    /// finished — completed, failed or cancelled — is not active, so its load id is not
    /// one the job list reports.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Loading)
    }

    /// The load status this state is REPORTED as, for a job carrying no run of its own.
    /// A job that ran reports the status its run ended in ([`LoadRun::reported_status`]).
    pub fn reported(self) -> &'static str {
        match self {
            Self::Queued => LOAD_IN_QUEUE,
            Self::Loading => LOAD_IN_PROGRESS,
            Self::Completed => LOAD_COMPLETED,
            Self::Failed => LOAD_FAILED,
            Self::Cancelled => LOAD_CANCELLED_BY_USER,
        }
    }
}

/// One entry of a job's error log: what the loader could not read, and where.
///
/// The members are the ones the loader's own error log carries — `errorCode`,
/// `errorMessage`, `fileName` and `recordNum` — so a client reads the same record it reads
/// from Neptune.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadErrorLog {
    pub error_code: String,
    pub error_message: String,
    /// The object the record was in, as the `s3://` location it was read from.
    pub file_name: String,
    /// Which record of that object it was, counted the way the object numbers its
    /// records; `0` for an error that belongs to the object as a whole.
    pub record_num: u64,
}

impl LoadErrorLog {
    /// An error log entry for `record_num` of `file_name`.
    pub fn new(
        error_code: &str,
        error_message: impl Into<String>,
        file_name: impl Into<String>,
        record_num: u64,
    ) -> Self {
        Self {
            error_code: error_code.to_string(),
            error_message: error_message.into(),
            file_name: file_name.into(),
            record_num,
        }
    }

    /// This entry as the loader's error log spells one.
    pub fn document(&self) -> Value {
        json!({
            "errorCode": self.error_code,
            "errorMessage": self.error_message,
            "fileName": self.file_name,
            "recordNum": self.record_num,
        })
    }
}

/// What ONE load did: where it read from, what it took, and what it could not read.
#[derive(Debug, Clone)]
pub struct LoadRun {
    /// The location the load read, as `overallStatus$fullUri` reports it.
    pub full_uri: String,
    /// The modelled `format` value the objects were read in.
    pub format: String,
    /// The modelled `mode` value the load ran in.
    pub mode: String,
    /// Which load of this source it is, counted from one: a source loaded again is a
    /// later run of it, which is what `overallStatus$runNumber` reports.
    pub run_number: u64,
    /// When the loader started reading.
    pub started_at: SystemTime,
    /// When the load reaches its terminal status — the instant the clock owes it.
    pub settles_at: SystemTime,
    /// The terminal state the load ends in once that instant has passed.
    pub outcome: LoaderJobState,
    /// The load status that terminal state is reported as.
    pub reported_status: String,
    /// How many records (rows or triples) the load read.
    pub total_records: u64,
    /// How many of those named an element the graph already held.
    pub total_duplicates: u64,
    /// How many records the loader could not read as the format the request named.
    pub parsing_errors: u64,
    /// How many values did not read as the type their column declared.
    pub datatype_mismatch_errors: u64,
    /// How many elements the graph would not take.
    pub insert_errors: u64,
    /// What each of those was, newest last and bounded by [`MAX_LOAD_ERROR_LOGS`].
    pub error_logs: Vec<LoadErrorLog>,
}

impl LoadRun {
    /// How many errors of every kind the run met.
    pub fn error_count(&self) -> u64 {
        self.parsing_errors
            .saturating_add(self.datatype_mismatch_errors)
            .saturating_add(self.insert_errors)
    }
}

/// One bulk-load job this engine has admitted, as this emulator persists it.
#[derive(Debug, Clone)]
pub struct LoaderJobRecord {
    /// The id the job is reached by — `GetLoaderJobStatus`'s and `CancelLoaderJob`'s
    /// `loadId` label, the value `ListLoaderJobs` lists, and the key this record is
    /// stored under.
    pub load_id: String,
    /// When the loader admitted the job. The job list orders by this instant, so a job
    /// the loader took on later is reported ahead of an older one.
    pub admitted_at: SystemTime,
    pub state: LoaderJobState,
    /// What the load this job is for DID. `None` for a job no run was recorded for, which
    /// reports its state's own status and no measurements.
    pub run: Option<LoadRun>,
    /// When the job reached its terminal state, which is what the time a load spent is
    /// counted to. `None` while it has not.
    pub finished_at: Option<SystemTime>,
}

impl LoaderJobRecord {
    /// A record for the job `load_id`, admitted at `admitted_at` in `state`.
    pub fn admitted(
        load_id: impl Into<String>,
        admitted_at: SystemTime,
        state: LoaderJobState,
    ) -> Self {
        Self {
            load_id: load_id.into(),
            admitted_at,
            state,
            run: None,
            finished_at: None,
        }
    }

    /// Materialise the terminal status the clock owes this job, as of `now`.
    ///
    /// A job with no run of its own is owed nothing, and a job the clock has not reached
    /// yet keeps the status it has: the transition is due at [`LoadRun::settles_at`], and
    /// it lands AT that instant rather than at the instant it was noticed, so two readers
    /// see one job finish at one time.
    pub fn settle(&mut self, now: SystemTime) {
        if !self.state.is_active() {
            return;
        }
        let Some(run) = self.run.as_ref() else {
            return;
        };
        if now < run.settles_at {
            return;
        }
        self.state = run.outcome;
        self.finished_at = Some(run.settles_at);
    }

    /// The load status this job reports: its run's terminal status once it has finished,
    /// and the status of the state it is in while it has not.
    pub fn reported_status(&self) -> String {
        match (&self.run, self.state.is_active()) {
            (Some(run), false) => run.reported_status.clone(),
            _ => self.state.reported().to_string(),
        }
    }

    /// How long the load has spent, in whole seconds: up to the instant it finished, or up
    /// to `now` while it is still running.
    fn time_spent(&self, now: SystemTime) -> u64 {
        let Some(run) = self.run.as_ref() else {
            return 0;
        };
        let until = self.finished_at.unwrap_or(now).max(run.started_at);
        until
            .duration_since(run.started_at)
            .unwrap_or_default()
            .as_secs()
    }

    /// The `overallStatus` record the loader reports for this job.
    fn overall_status(&self, now: SystemTime) -> Value {
        let status = self.reported_status();
        let Some(run) = self.run.as_ref() else {
            return json!({
                "fullUri": "",
                "runNumber": 1,
                "retryNumber": 0,
                "status": status,
                "totalTimeSpent": 0,
                "startTime": epoch_seconds(self.admitted_at),
                "totalRecords": 0,
                "totalDuplicates": 0,
                "parsingErrors": 0,
                "datatypeMismatchErrors": 0,
                "insertErrors": 0,
            });
        };
        json!({
            "fullUri": run.full_uri,
            "runNumber": run.run_number,
            // Nothing retries a load here: a load that failed is reported as it ran, and a
            // caller that wants it again starts one.
            "retryNumber": 0,
            "status": status,
            "totalTimeSpent": self.time_spent(now),
            "startTime": epoch_seconds(run.started_at),
            "totalRecords": run.total_records,
            "totalDuplicates": run.total_duplicates,
            "parsingErrors": run.parsing_errors,
            "datatypeMismatchErrors": run.datatype_mismatch_errors,
            "insertErrors": run.insert_errors,
        })
    }

    /// The whole status payload `GetLoaderJobStatus` answers for this job.
    ///
    /// `feedCount` and `overallStatus` are always reported; `details` adds the feeds the
    /// load failed on, and `errors` adds one PAGE of the job's error log, as the members
    /// that ask for them document ("include details beyond the overall status" and
    /// "include a list of errors encountered ... The list of errors is paged").
    pub fn status_payload(
        &self,
        now: SystemTime,
        details: bool,
        errors: bool,
        page: i32,
        errors_per_page: i32,
    ) -> Value {
        let status = self.reported_status();
        let overall = self.overall_status(now);
        // Built as the map itself, so the payload is assembled rather than reached into:
        // nothing here can fail on the way to the answer.
        let mut members = ::serde_json::Map::new();
        // One load reads one source, so the engine counted one feed in that status.
        members.insert(
            "feedCount".to_string(),
            json!([ { status.clone(): 1 } ]),
        );
        members.insert("overallStatus".to_string(), overall.clone());
        if details {
            // The feeds a load failed on: this engine loads one feed per job, so the list
            // holds that feed exactly when the load did not complete, and nothing
            // otherwise.
            let failed = if matches!(self.state, LoaderJobState::Completed) {
                Vec::new()
            } else {
                vec![overall]
            };
            members.insert("failedFeeds".to_string(), Value::Array(failed));
        }
        if errors {
            members.insert(
                "errors".to_string(),
                self.error_page(page, errors_per_page),
            );
        }
        Value::Object(members)
    }

    /// One page of this job's error log, as the `errors` member reports it.
    ///
    /// `startIndex` and `endIndex` are the positions of the page in the whole log, counted
    /// from one, so a caller can page through it; a page past the end of the log carries
    /// no entry.
    fn error_page(&self, page: i32, errors_per_page: i32) -> Value {
        let logs: &[LoadErrorLog] = self
            .run
            .as_ref()
            .map_or(&[], |run| run.error_logs.as_slice());
        let page = usize::try_from(page.max(DEFAULT_ERROR_PAGE)).unwrap_or(1);
        let per_page = usize::try_from(errors_per_page.max(1)).unwrap_or(1);
        let start = page.saturating_sub(1).saturating_mul(per_page);
        let entries: Vec<Value> = logs
            .iter()
            .skip(start)
            .take(per_page)
            .map(LoadErrorLog::document)
            .collect();
        json!({
            "startIndex": start.saturating_add(1),
            "endIndex": start.saturating_add(entries.len()),
            "loadId": self.load_id,
            "errorLogs": entries,
        })
    }
}

/// An instant as the loader's status reports one: whole seconds since the epoch, which is
/// the form `overallStatus$startTime` states an instant in.
fn epoch_seconds(instant: SystemTime) -> u64 {
    instant
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The facts of one load request the loader acts on, read off the request once.
#[derive(Debug, Clone)]
pub struct LoadRequest {
    /// The location the load reads, as the request spelled it.
    pub source: String,
    /// The modelled `format` value the objects are read in.
    pub format: String,
    /// The Region the source bucket is in, which is the Region the object store is reached
    /// in.
    pub region: String,
    /// The modelled `mode` value.
    pub mode: String,
    /// Whether the load stops on the first error it meets.
    pub fail_on_error: bool,
    /// Whether a new value for a property an element already carries replaces it.
    pub update_single_cardinality_properties: bool,
    /// Whether the load reads its files in the order they appear rather than loading the
    /// vertex files first.
    pub edge_only_load: bool,
    /// Whether the load data names its relationship ids itself.
    pub user_provided_edge_ids: bool,
    /// Whether an empty cell is a value rather than nothing.
    pub allow_empty_strings: bool,
    /// The base IRI relative IRIs of an RDF object resolve against.
    pub base_uri: String,
    /// The graph an RDF statement carrying no graph of its own belongs to.
    pub named_graph_uri: String,
}

/// `parserConfiguration`'s member naming the base IRI "for RDF/XML and Turtle formats".
pub const BASE_URI_PARAMETER: &str = "baseUri";

/// Its documented default: "The default is https://aws.amazon.com/neptune/default".
pub const DEFAULT_BASE_URI: &str = "https://aws.amazon.com/neptune/default";

/// `parserConfiguration`'s member naming "the default graph for all RDF formats when no
/// graph is specified".
pub const NAMED_GRAPH_URI_PARAMETER: &str = "namedGraphUri";

/// Its documented default: "The default is
/// https://aws.amazon.com/neptune/vocab/v01/DefaultNamedGraph".
pub const DEFAULT_NAMED_GRAPH_URI: &str =
    "https://aws.amazon.com/neptune/vocab/v01/DefaultNamedGraph";

/// `parserConfiguration`'s member deciding whether an empty CSV cell is a value: "If
/// allowEmptyStrings is set to false (the default), such empty strings are treated as
/// nulls".
pub const ALLOW_EMPTY_STRINGS_PARAMETER: &str = "allowEmptyStrings";

impl LoadRequest {
    /// The load `input` asks for, with every documented default the request left out.
    pub fn of(input: &StartLoaderJobInput) -> Self {
        let parameter = |name: &str| {
            input
                .parser_configuration
                .as_ref()
                .and_then(|configuration| configuration.get(name))
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        Self {
            source: input.source.clone(),
            format: input.format.clone(),
            region: input.s3_bucket_region.clone(),
            mode: input.mode.clone().unwrap_or_else(|| DEFAULT_MODE.to_string()),
            // "failOnError ... Default value: TRUE."
            fail_on_error: input.fail_on_error.unwrap_or(true),
            // "By default, or when updateSingleCardinalityProperties is explicitly set to
            // FALSE, the loader treats a new value as an error".
            update_single_cardinality_properties: input
                .update_single_cardinality_properties
                .unwrap_or(false),
            // "edgeOnlyLoad ... Default value: FALSE."
            edge_only_load: input.edge_only_load.unwrap_or(false),
            // "When userProvidedEdgeIds is absent or set to True, an :ID column must be
            // present in every relationship file in the load."
            user_provided_edge_ids: input.user_provided_edge_ids.unwrap_or(true),
            allow_empty_strings: parameter(ALLOW_EMPTY_STRINGS_PARAMETER)
                .and_then(|value| value.parse::<bool>().ok())
                .unwrap_or(false),
            base_uri: parameter(BASE_URI_PARAMETER)
                .unwrap_or(DEFAULT_BASE_URI)
                .to_string(),
            named_graph_uri: parameter(NAMED_GRAPH_URI_PARAMETER)
                .unwrap_or(DEFAULT_NAMED_GRAPH_URI)
                .to_string(),
        }
    }
}

/// A fresh load id: the `loadId` `StartLoaderJob` hands back, which the status and cancel
/// endpoints address the job by.
///
/// Version 4, so the id is derived from nothing the caller sent: no caller can guess the
/// id of somebody else's load, which is what a cancel stops.
#[must_use]
pub fn new_load_id() -> String {
    ::uuid::Uuid::new_v4().to_string()
}

impl super::ComAmazonawsNeptunedataAmazonneptunedataplaneState {
    /// Materialise every loader-job transition the clock has made due, as of `now`.
    ///
    /// The ONE place a load's terminal status lands, called by each operation that reports
    /// on or stops a job: what it writes was owed at `now` whether or not anybody asked,
    /// so a read that calls it changes nothing a later caller would not have seen anyway.
    pub fn settle_loader_jobs(&mut self, now: SystemTime) {
        for job in self.loader_jobs.values_mut() {
            job.settle(now);
        }
    }

    /// How many loads of `source` this engine has already run, which is what the run
    /// number of the next one counts from.
    fn loads_of_source(&self, source: &str) -> u64 {
        self.loader_jobs
            .values()
            .filter(|job| job.run.as_ref().is_some_and(|run| run.full_uri == source))
            .count() as u64
    }

    /// Whether a load of `source` has already COMPLETED, which is what `AUTO` and `RESUME`
    /// look for: "If a previous load job loaded all files from the same source
    /// successfully, nothing is reloaded, and the loader returns success."
    fn source_already_loaded(&self, source: &str) -> bool {
        self.loader_jobs.values().any(|job| {
            matches!(job.state, LoaderJobState::Completed)
                && job.run.as_ref().is_some_and(|run| run.full_uri == source)
        })
    }

    /// Admit the load `request` asks for, write what `reading` read into this engine's
    /// graph, and record the job under `load_id`.
    ///
    /// This is where a load becomes state: the elements the reading holds are inserted
    /// into the one graph every reader of this engine answers from, the counts the job
    /// reports are the counts of THAT write, and the job's terminal status is made due
    /// [`LOAD_SETTLES_AFTER`] from `now`.
    pub fn admit_load(
        &mut self,
        load_id: String,
        now: SystemTime,
        request: &LoadRequest,
        reading: LoadReading,
    ) -> LoaderJobRecord {
        let run_number = self.loads_of_source(&request.source).saturating_add(1);
        let mut run = LoadRun {
            full_uri: request.source.clone(),
            format: request.format.clone(),
            mode: request.mode.clone(),
            run_number,
            started_at: now,
            settles_at: now + LOAD_SETTLES_AFTER,
            outcome: LoaderJobState::Completed,
            reported_status: LOAD_COMPLETED.to_string(),
            total_records: reading.records,
            total_duplicates: 0,
            parsing_errors: reading.parsing_errors,
            datatype_mismatch_errors: reading.datatype_mismatch_errors,
            insert_errors: 0,
            error_logs: reading.error_logs,
        };
        match reading.halted {
            // The load could not be carried out at all, so nothing it had read is written:
            // the status says why and the graph is left exactly as it was.
            Some(status) => {
                run.outcome = LoaderJobState::Failed;
                run.reported_status = status.to_string();
            }
            None => {
                let incomplete = reading.incomplete;
                let applied = apply_load(&mut self.graph, reading.elements, request);
                run.total_duplicates = applied.duplicates;
                run.insert_errors = applied.insert_errors;
                push_error_logs(&mut run.error_logs, applied.error_logs);
                // A load did not COMPLETE unless it read everything its source held and
                // met no error it was told to stop on: `failOnError` is TRUE by default —
                // "the loader stops as soon as it encounters an error" — and a load that
                // stopped on a ceiling of this emulator's own, or could not fetch one of
                // its objects, did not finish either.
                if incomplete || (request.fail_on_error && run.error_count() > 0) {
                    run.outcome = LoaderJobState::Failed;
                    run.reported_status = LOAD_FAILED.to_string();
                }
            }
        }
        let record = LoaderJobRecord {
            load_id: load_id.clone(),
            admitted_at: now,
            state: LoaderJobState::Loading,
            run: Some(run),
            finished_at: None,
        };
        self.loader_jobs.insert(load_id, record.clone());
        self.forget_oldest_loader_jobs();
        record
    }

    /// Stop the load `load_id` names, and report the job as it stands afterwards.
    ///
    /// `None` for an id that names no load the loader still owes an outcome: one the
    /// registry does not hold, and one whose job has already finished — there is nothing
    /// left to stop in either case, which is what the cancel endpoint refuses.
    pub fn cancel_loader_job(
        &mut self,
        load_id: &str,
        now: SystemTime,
    ) -> Option<LoaderJobRecord> {
        let job = self.loader_jobs.get_mut(load_id)?;
        job.settle(now);
        if !job.state.is_active() {
            return None;
        }
        job.state = LoaderJobState::Cancelled;
        job.finished_at = Some(now);
        if let Some(run) = job.run.as_mut() {
            run.outcome = LoaderJobState::Cancelled;
            run.reported_status = LOAD_CANCELLED_BY_USER.to_string();
        }
        Some(job.clone())
    }

    /// Forget the oldest jobs past [`LOADER_JOB_HISTORY`], so the registry keeps the most
    /// recent loads and no more.
    fn forget_oldest_loader_jobs(&mut self) {
        if self.loader_jobs.len() <= LOADER_JOB_HISTORY {
            return;
        }
        let mut admitted: Vec<(SystemTime, String)> = self
            .loader_jobs
            .values()
            .map(|job| (job.admitted_at, job.load_id.clone()))
            .collect();
        // Oldest first, ties broken by load id, so the same jobs are forgotten whatever
        // order the registry is walked in.
        admitted.sort();
        let surplus = self.loader_jobs.len() - LOADER_JOB_HISTORY;
        for (_, load_id) in admitted.iter().take(surplus) {
            self.loader_jobs.remove(load_id);
        }
    }

    /// Whether the mode `request` names makes this load read its source at all.
    ///
    /// `NEW` "creates a new load request regardless of any previous loads"; `AUTO` and
    /// `RESUME` look for a previous load of the same source and reload nothing when one
    /// completed. A `RESUME` that finds none is a load the loader stops, which is this
    /// answer's `Err`.
    pub fn resolve_load_mode(&self, request: &LoadRequest) -> Result<LoadPlan, &'static str> {
        if request.mode.eq_ignore_ascii_case(MODE_NEW) {
            return Ok(LoadPlan::ReadSource);
        }
        if self.source_already_loaded(&request.source) {
            return Ok(LoadPlan::NothingToReload);
        }
        if request.mode.eq_ignore_ascii_case(MODE_RESUME) {
            // "If no previous load job is found, the loader stops."
            return Err(RESUME_NOT_POSSIBLE_CODE);
        }
        Ok(LoadPlan::ReadSource)
    }
}

/// What the mode a request named makes of the load: read the source, or reload nothing
/// because a previous load of the same source already took it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadPlan {
    /// Read the source and load what it holds.
    ReadSource,
    /// A previous load of this source completed, so nothing is reloaded.
    NothingToReload,
}

/// Append `logs` to `into`, keeping it bounded by [`MAX_LOAD_ERROR_LOGS`].
pub fn push_error_logs(into: &mut Vec<LoadErrorLog>, logs: Vec<LoadErrorLog>) {
    for log in logs {
        if into.len() >= MAX_LOAD_ERROR_LOGS {
            return;
        }
        into.push(log);
    }
}

/// What writing one load's elements into the graph did.
#[derive(Debug, Default)]
pub struct AppliedLoad {
    /// How many records named an element the graph already held.
    pub duplicates: u64,
    /// How many elements the graph would not take.
    pub insert_errors: u64,
    /// What each of those was.
    pub error_logs: Vec<LoadErrorLog>,
}

/// Write the elements one load read into `graph`, and report what that did.
///
/// The VERTEX files are applied before the edge files unless the request asked for the
/// files to be read in the order they appear: "When this parameter is set to FALSE, the
/// loader automatically loads vertex files first, then edge files afterwards ... When this
/// parameter is set to TRUE, the loader skips the initial scanning phase and immediately
/// loads all files in the order they appear" — and an edge whose endpoints are not in the
/// graph yet is an error of that edge, exactly as the store's own rule makes it.
pub fn apply_load(
    graph: &mut super::PropertyGraph,
    elements: Vec<LoadedElement>,
    request: &LoadRequest,
) -> AppliedLoad {
    let mut applied = AppliedLoad::default();
    let ordered: Vec<LoadedElement> = if request.edge_only_load {
        elements
    } else {
        let (nodes, edges): (Vec<LoadedElement>, Vec<LoadedElement>) = elements
            .into_iter()
            .partition(|element| matches!(element, LoadedElement::Node { .. }));
        nodes.into_iter().chain(edges).collect()
    };
    for element in ordered {
        match element {
            LoadedElement::Node {
                id,
                labels,
                properties,
                file_name,
                record_num,
            } => {
                apply_node(
                    graph,
                    &mut applied,
                    request,
                    &id,
                    labels,
                    properties,
                    &file_name,
                    record_num,
                );
            }
            LoadedElement::Edge {
                id,
                rel_type,
                from,
                to,
                properties,
                file_name,
                record_num,
            } => {
                apply_edge(
                    graph,
                    &mut applied,
                    &id,
                    rel_type,
                    &from,
                    &to,
                    properties,
                    &file_name,
                    record_num,
                );
            }
        }
    }
    applied
}

/// Write one node of a load into the graph.
///
/// A record naming an id the graph already holds is a DUPLICATE: the loader updates the
/// element rather than creating a second one, and whether a new value for a property it
/// already carries replaces that value is what `updateSingleCardinalityProperties`
/// decides — "By default ... the loader treats a new value as an error, because it
/// violates single cardinality. When updateSingleCardinalityProperties is set to TRUE ...
/// the bulk loader replaces the existing value with the new one."
#[allow(clippy::too_many_arguments)]
fn apply_node(
    graph: &mut super::PropertyGraph,
    applied: &mut AppliedLoad,
    request: &LoadRequest,
    id: &str,
    labels: Vec<String>,
    properties: BTreeMap<String, Value>,
    file_name: &str,
    record_num: u64,
) {
    if graph.node(id).is_none() {
        if let Err(error) = graph.insert_node(Some(id.to_string()), labels, properties) {
            applied.note(
                INSERT_ERROR_CODE,
                format!("The vertex was not loaded: {}", written_error(&error)),
                file_name,
                record_num,
            );
        }
        return;
    }
    // A record that restates a vertex the graph already holds without bringing any data to
    // it — the endpoint a statement names, say — duplicates nothing: there is nothing of it
    // to load twice, so it is not counted against the records the source repeated.
    if labels.is_empty() && properties.is_empty() {
        return;
    }
    applied.duplicates = applied.duplicates.saturating_add(1);
    for label in labels {
        if let Err(error) = graph.add_node_label(id, &label) {
            applied.note(
                INSERT_ERROR_CODE,
                format!("A label was not added to the vertex: {}", written_error(&error)),
                file_name,
                record_num,
            );
        }
    }
    for (key, value) in properties {
        let held = graph
            .node(id)
            .and_then(|node| node.properties.get(&key))
            .cloned();
        match held {
            Some(held) if held != value && !request.update_single_cardinality_properties => {
                applied.note(
                    INSERT_ERROR_CODE,
                    format!(
                        "A new value for the single-cardinality property {key:?} was not \
                         loaded, because updateSingleCardinalityProperties is not set."
                    ),
                    file_name,
                    record_num,
                );
            }
            _ => {
                if let Err(error) = graph.set_element_property(id, &key, Some(value)) {
                    applied.note(
                        INSERT_ERROR_CODE,
                        format!("A property was not set: {}", written_error(&error)),
                        file_name,
                        record_num,
                    );
                }
            }
        }
    }
}

/// Write one edge of a load into the graph.
#[allow(clippy::too_many_arguments)]
fn apply_edge(
    graph: &mut super::PropertyGraph,
    applied: &mut AppliedLoad,
    id: &Option<String>,
    rel_type: String,
    from: &str,
    to: &str,
    properties: BTreeMap<String, Value>,
    file_name: &str,
    record_num: u64,
) {
    // An edge the graph already holds under this id is the edge the load read: the same
    // statement loaded twice is one edge, so it is counted as a duplicate rather than
    // written again.
    if let Some(id) = id.as_deref() {
        if graph.relationship(id).is_some() {
            applied.duplicates = applied.duplicates.saturating_add(1);
            return;
        }
    }
    for endpoint in [from, to] {
        if graph.node(endpoint).is_none() {
            applied.note(
                FROM_OR_TO_VERTEX_MISSING_CODE,
                format!(
                    "The edge joins the vertex {:?}, which the load did not create.",
                    super::echoed(endpoint)
                ),
                file_name,
                record_num,
            );
            return;
        }
    }
    if let Err(error) = graph.insert_relationship(id.clone(), rel_type, from, to, properties) {
        applied.note(
            INSERT_ERROR_CODE,
            format!("The edge was not loaded: {}", written_error(&error)),
            file_name,
            record_num,
        );
    }
}

impl AppliedLoad {
    /// Count one element the graph would not take, and log what happened to it.
    fn note(&mut self, code: &str, message: String, file_name: &str, record_num: u64) {
        self.insert_errors = self.insert_errors.saturating_add(1);
        if self.error_logs.len() < MAX_LOAD_ERROR_LOGS {
            self.error_logs
                .push(LoadErrorLog::new(code, message, file_name, record_num));
        }
    }
}

/// Why the graph would not take an element, as an error log states it. The ceilings are
/// this emulator's own, so each sentence states the ceiling it met and quotes nothing out
/// of the data.
fn written_error(error: &super::GraphWriteError) -> String {
    use super::GraphWriteError;
    match error {
        GraphWriteError::NodeLimit { ceiling } => {
            format!("this engine's graph holds at most {ceiling} vertices")
        }
        GraphWriteError::RelationshipLimit { ceiling } => {
            format!("this engine's graph holds at most {ceiling} edges")
        }
        GraphWriteError::ByteLimit { ceiling } => {
            format!("this engine's graph holds at most {ceiling} bytes of element data")
        }
        GraphWriteError::LabelLimit { ceiling } => {
            format!("one vertex carries at most {ceiling} labels")
        }
        GraphWriteError::NameTooLong { ceiling, .. } => {
            format!("an id, a label or an edge type is at most {ceiling} bytes")
        }
        GraphWriteError::DuplicateId { .. } => {
            "the id names an element the graph already holds".to_string()
        }
        GraphWriteError::MissingEndpoint { .. } => {
            "an endpoint names a vertex the graph does not hold".to_string()
        }
    }
}
