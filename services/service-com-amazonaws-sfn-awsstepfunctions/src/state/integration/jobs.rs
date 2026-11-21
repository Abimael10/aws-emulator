// SPDX-License-Identifier: CC0-1.0

//! The CATALOGUE of run-a-job (`.sync`) service integrations: which integrations this
//! emulator performs that pattern for, and the facts of the invoked service's own API
//! each one's three calls are made from.
//!
//! Split out of `super`'s file so that file stays inside the project's length limit;
//! the catalogue and the readings over it are unchanged by the move. The pattern is
//! PERFORMED in `super::super::run_a_job`, which polls the job an entry here names the
//! reporting call of.

use super::*;

/// One run-a-job (`.sync`) service integration: the three calls the pattern is made
/// of, and where the job's identifier, status and failure sit in their documents.
///
/// The pattern itself is not in any Smithy model — it is Step Functions' own
/// behaviour over another service's API — so each entry names the operations and
/// members of the INVOKED service's model, cited on the entry. Only integrations
/// whose reporting and stopping calls this emulator can really make have an entry:
/// an integration with none is reported by [`read`] as one no job was started for,
/// rather than parked at a job nothing could poll.
#[derive(Debug)]
pub struct RunAJob {
    /// The `<service>:<action>` of the `Resource` that starts the job, as the
    /// optimized integration spells it.
    pub starts: &'static str,
    /// The member of the STARTING call's response that identifies the job.
    pub job_id: &'static str,
    /// The operation that reports the job, and the member of its request the
    /// identifier goes in.
    pub describe: &'static str,
    pub describe_member: &'static str,
    /// Where the job's status sits in the reporting call's response.
    pub status_at: &'static [&'static str],
    /// Where the document the TASK REPORTS sits in that same response: the member the
    /// integration answers with, and empty for an integration that answers with the
    /// whole response.
    ///
    /// Step Functions reports what the invoked service said about the job, and for an
    /// operation whose answer WRAPS the job's own document — Glue's `GetJobRun`, whose
    /// response carries the run under `JobRun` — what it reports is that document
    /// rather than the wrapper, so a task's `ResultPath` and a `Catch` reading the
    /// failure's cause address the job's own members (`Id`, `JobRunState`) directly.
    /// An operation whose answer IS the job's document carries no wrapper to unwrap.
    pub reports_at: &'static [&'static str],
    /// The status values that mean the job finished its work, and those that mean it
    /// ended without finishing it. A status in neither list is the job still running.
    pub succeeded: &'static [&'static str],
    pub failed: &'static [&'static str],
    /// Where the reporting call's response carries a document of the job's own as a
    /// JSON-encoded STRING — its input, its output — each of which the pattern's
    /// second revision reports as the parsed document; empty where that service
    /// carries none.
    pub json_text_at: &'static [&'static [&'static str]],
    /// The operation that stops the job, and the member of its request the
    /// identifier goes in.
    pub stop: &'static str,
    pub stop_member: &'static str,
    /// Whether the stopping call's identifier member is a LIST of identifiers, as
    /// that operation's own model types it — a batch stop takes the ids of several
    /// jobs, and the one job a task started is a batch of one.
    pub stops_in_batch: bool,
    /// The members of the STARTING call's REQUEST that the reporting and stopping
    /// calls carry BESIDE the job's identifier, because those operations do not name
    /// the job by that identifier alone: each is a member of the job's own identity
    /// rather than of the run the identifier names. Named in the integration's own
    /// PascalCase, and empty for a job its identifier names on its own.
    pub names_job_with: &'static [&'static str],
    /// The members of the REPORTING call's response that carry an INSTANT, which the
    /// task's result renders in the SDK's own date type — whole epoch milliseconds,
    /// exactly as the STARTING call's response does
    /// ([`EPOCH_MILLIS_MEMBERS`]). Empty for an integration whose report carries none
    /// at its top level.
    pub instants_at: &'static [&'static str],
    /// The members of the job's own document the FAILURE cause does not carry, named
    /// as the reporting call's response spells them.
    ///
    /// RECORDED: the cause a failed job's task reports is the invoked service's report
    /// of the job with the job's own INPUT redacted — a nested execution's `input` and
    /// the detail that says whether it was included — because that cause travels into
    /// the parent's history and its failure, where the data the child was handed is not
    /// repeated. Empty for an integration whose report carries no such document.
    pub redacted_in_cause: &'static [&'static str],
    /// Where the resource the pattern's OWN calls are taken on is named, for the
    /// access request each is subject to ([`JobTarget`]); `None` for an integration
    /// whose target resource is not stated, whose calls are then made as before.
    pub acts_on: Option<JobTarget>,
}

/// Where the resource the calls that REPORT and STOP a job are taken on is named.
///
/// Those calls are made as the execution role too, so each is subject to an access
/// request — and the resource that request is over is not in the call's own request:
/// a reporting operation names the JOB by its identifier, while IAM takes the action
/// on the resource the job belongs to. Which resource that is, is the integration's
/// own fact, so it is stated per entry.
#[derive(Debug, Clone, Copy)]
pub enum JobTarget {
    /// The job's own identifier IS the resource's ARN, so the pattern's calls are
    /// taken on the thing the starting call minted (a nested execution).
    Identifier,
    /// The job belongs to the resource the STARTING integration acts on, named by the
    /// `<namespace>:<action>` of that integration: its [`TARGET_RESOURCES`] entry says
    /// which member of the starting request names it and how (a query's workgroup, a
    /// run's job).
    Started(&'static str),
}

/// The members of the starting request that name the job, beside its own identifier
/// ([`RunAJob::names_job_with`]), as the calls that report and stop it carry them.
pub type JobNaming = ::std::collections::BTreeMap<String, String>;

impl RunAJob {
    /// The members of the starting call's `request` that the calls which REPORT and
    /// STOP the job carry beside its identifier.
    ///
    /// Each is read under both spellings a definition may write it in — the
    /// integration's own PascalCase and the invoked model's ([`MemberCase`]) — since
    /// either reaches the operation ([`wire_request`]), and is carried on under the
    /// name this entry gives it. A member the request did not carry is not invented:
    /// the invoked operation answers for a request that omits one, which is its own
    /// answer to give.
    pub fn naming(&self, request: &Value) -> JobNaming {
        let mut naming = JobNaming::new();
        for member in self.names_job_with {
            let named = [MemberCase::Pascal, MemberCase::LowerCamel]
                .into_iter()
                .find_map(|case| request.get(case.spell(member)))
                .and_then(Value::as_str);
            if let Some(value) = named {
                naming.insert((*member).to_string(), value.to_string());
            }
        }
        naming
    }

    /// How the request that STARTED the job addresses the resource the pattern's own
    /// calls are taken on ([`JobTarget`]), and `None` for an integration that states
    /// none or a request that named none.
    ///
    /// Read where the job is STARTED, because that request is where the resource is
    /// named: the calls that report and stop the job carry the job's identifier
    /// instead, so what they are authorized over is carried with the parked job
    /// ([`crate::state::JobTask::acts_on`]).
    pub fn acts_on_named(&self, request: &Value, job_id: &str) -> Option<String> {
        match self.acts_on? {
            JobTarget::Identifier => Some(job_id.to_string()),
            JobTarget::Started(integration) => TARGET_RESOURCES
                .iter()
                .find(|entry| entry.integration == integration)?
                .named_in(request),
        }
    }

    /// The ARN of that resource, as addressed in the Region `region` the pattern's
    /// calls are made in ([`TargetNaming::arn_of`]).
    pub fn acts_on_arn(&self, named: &str, region: &str) -> Option<String> {
        match self.acts_on? {
            JobTarget::Identifier => TargetNaming::Arn.arn_of(named, region),
            JobTarget::Started(integration) => TARGET_RESOURCES
                .iter()
                .find(|entry| entry.integration == integration)?
                .named
                .arn_of(named, region),
        }
    }

    /// Whether `status` is one the job ENDED on, and whether it ended having done its
    /// work: `None` while the job is still running.
    pub(super) fn finished(&self, status: &str) -> Option<bool> {
        if self.succeeded.contains(&status) {
            return Some(true);
        }
        if self.failed.contains(&status) {
            return Some(false);
        }
        None
    }

    /// The value `path` names in `document`, as text.
    pub(super) fn text_at(document: &Value, path: &[&str]) -> Option<String> {
        let mut node = document;
        for member in path {
            node = node.get(*member)?;
        }
        match node {
            Value::String(text) => Some(text.clone()),
            Value::Null => None,
            other => Some(other.to_string()),
        }
    }

    /// The job's status, as the reporting call's `answer` carries it.
    pub fn status_of(&self, answer: &Value) -> Option<String> {
        Self::text_at(answer, self.status_at)
    }

    /// Whether the job `answer` reports has ENDED, and with what: `Ok(answer)` for
    /// one that did its work, `Err(the cause its task fails with)` for one that did
    /// not, and `None` while it is still running.
    ///
    /// ONE document answers for the job whichever way it ended: the invoked service's
    /// own report of it. A job that did its work makes the task succeed WITH that
    /// report, and a job that ended without doing its work makes the task fail with
    /// that same report as the failure's `cause` — the JSON text of it, in the
    /// integration's own convention ([`task_result`]), because `cause` is a string.
    /// So a `Catch` reading why a job failed reads the whole of what the service said
    /// about it — its status, its identity and the error and reason it ended on —
    /// rather than one member of it picked out here.
    pub fn outcome_of(&self, answer: &Value, parsed_output: bool) -> Option<Result<Value, String>> {
        let status = self.status_of(answer)?;
        let reported = self.reported(answer, parsed_output);
        match self.finished(&status)? {
            true => Some(Ok(reported)),
            false => Some(Err(task_result(&self.redacted(reported)).to_string())),
        }
    }

    /// `reported` without the members a failure's cause does not carry
    /// ([`RunAJob::redacted_in_cause`]).
    fn redacted(&self, mut reported: Value) -> Value {
        if let Some(members) = reported.as_object_mut() {
            for member in self.redacted_in_cause {
                members.remove(*member);
            }
        }
        reported
    }

    /// The document the task reports as its result: what the reporting call said about
    /// the job — the member of its response that carries the job's own document
    /// ([`RunAJob::reports_at`]) — with each document the job carries as JSON TEXT
    /// parsed in place for the pattern's second revision.
    ///
    /// A response the wrapper is absent from is reported whole: the task answers with
    /// what the service said, so nothing is invented where the member it names is not
    /// there.
    pub(super) fn reported(&self, answer: &Value, parsed_output: bool) -> Value {
        let mut reported = answer.clone();
        if parsed_output {
            for path in self.json_text_at {
                parse_in_place(&mut reported, path);
            }
        }
        let mut wrapped = &reported;
        for member in self.reports_at {
            match wrapped.get(*member) {
                Some(inner) => wrapped = inner,
                None => return reported,
            }
        }
        wrapped.clone()
    }
}

/// Replace the JSON TEXT `path` names in `document` with the document that text
/// holds, leaving `document` as it is where the path names nothing or names text
/// that is not JSON.
pub(super) fn parse_in_place(document: &mut Value, path: &[&str]) {
    let Some((last, leading)) = path.split_last() else {
        return;
    };
    let mut node = document;
    for member in leading {
        match node.get_mut(*member) {
            Some(next) => node = next,
            None => return,
        }
    }
    let parsed = node
        .get(*last)
        .and_then(Value::as_str)
        .and_then(|text| ::serde_json::from_str::<Value>(text).ok());
    if let (Some(parsed), Some(object)) = (parsed, node.as_object_mut()) {
        object.insert((*last).to_string(), parsed);
    }
}

/// The run-a-job integrations this emulator performs.
///
/// One entry per `<service>:<action>` that STARTS a job, naming the calls that
/// report and stop it. A `.sync` `Resource` outside this list starts nothing: see
/// [`read`].
pub const RUN_A_JOB: &[RunAJob] = &[
    // A nested execution of a state machine, through this service's own API:
    // `StartExecution` answers `executionArn`, `DescribeExecution` reports `status`
    // (`ExecutionStatus`: RUNNING, SUCCEEDED, FAILED, TIMED_OUT, ABORTED) beside
    // `error`, `cause` and the string `output`, and `StopExecution` stops it
    // (`smithy_query(query="operation",operation="StartExecution,DescribeExecution,
    // StopExecution")` on this crate's own model).
    RunAJob {
        starts: "states:startExecution",
        job_id: "executionArn",
        describe: "DescribeExecution",
        describe_member: "executionArn",
        status_at: &["status"],
        // `DescribeExecution`'s answer IS the execution's own document, so there is no
        // wrapper to unwrap.
        reports_at: &[],
        succeeded: &[crate::state::EXECUTION_STATUS_SUCCEEDED],
        failed: &[
            crate::state::EXECUTION_STATUS_FAILED,
            crate::state::EXECUTION_STATUS_TIMED_OUT,
            crate::state::EXECUTION_STATUS_ABORTED,
        ],
        // `DescribeExecution` carries both the execution's `input` and its `output`
        // as "the string that contains the JSON input data" and the JSON output of
        // the execution.
        json_text_at: &[&["input"], &["output"]],
        stop: "StopExecution",
        stop_member: "executionArn",
        stops_in_batch: false,
        // `DescribeExecution` and `StopExecution` each name the execution by its own
        // ARN alone.
        names_job_with: &[],
        // `DescribeExecution` reports the instants the execution began and ended at,
        // "the date the execution is started" and "stopped".
        instants_at: &["startDate", "stopDate"],
        // The cause a failed child's task reports carries what the child ENDED as
        // without the input it was handed: `input` and the `inputDetails` that says
        // whether it was included.
        redacted_in_cause: &["input", "inputDetails"],
        // Both of those calls are taken on the EXECUTION the start minted, whose ARN
        // is the identifier it answered.
        acts_on: Some(JobTarget::Identifier),
    },
    // An Athena query: `StartQueryExecution` answers `QueryExecutionId`,
    // `GetQueryExecution` reports `QueryExecution.Status.State`
    // (`QueryExecutionState`: QUEUED, RUNNING, SUCCEEDED, FAILED, CANCELLED) beside
    // `StateChangeReason`, and `StopQueryExecution` stops it
    // (`smithy_query(query="operation",operation="StartQueryExecution,
    // GetQueryExecution,StopQueryExecution",model_path="services/
    // service-com-amazonaws-athena-amazonathena/smithy-model/
    // com-amazonaws-athena-amazonathena.json")`).
    RunAJob {
        starts: "athena:startQueryExecution",
        job_id: "QueryExecutionId",
        describe: "GetQueryExecution",
        describe_member: "QueryExecutionId",
        status_at: &["QueryExecution", "Status", "State"],
        // `GetQueryExecution` answers `{"QueryExecution": …}`, which is the document
        // this integration reports — the task's result carries the wrapper, as the
        // service's own answer does.
        reports_at: &[],
        succeeded: &["SUCCEEDED"],
        failed: &["FAILED", "CANCELLED"],
        json_text_at: &[],
        stop: "StopQueryExecution",
        stop_member: "QueryExecutionId",
        stops_in_batch: false,
        // `GetQueryExecution` and `StopQueryExecution` each name the query by its own
        // id alone.
        names_job_with: &[],
        // `GetQueryExecution` carries the query's instants nested inside
        // `QueryExecution.Status`, which is not a member of the report's top level.
        instants_at: &[],
        // The report of a query carries no document of the query's own.
        redacted_in_cause: &[],
        // Both of those calls are taken on the WORKGROUP the query runs in, which the
        // starting request named ([`TARGET_RESOURCES`]).
        acts_on: Some(JobTarget::Started("athena:startQueryExecution")),
    },
    // A Glue job run: `StartJobRun` answers `JobRunId`, `GetJobRun` reports
    // `JobRun.JobRunState` (`JobRunState`: STARTING, RUNNING, STOPPING, STOPPED,
    // SUCCEEDED, FAILED, TIMEOUT, ERROR, WAITING, EXPIRED) beside
    // `JobRun.ErrorMessage`, and `BatchStopJobRun` stops it. Both of those name the
    // run by the `JobName` of the job it is a run of — `@required` on each of them —
    // beside the run's own id, which `GetJobRun` takes as `RunId` and
    // `BatchStopJobRun` as the `JobRunIds` LIST
    // (`smithy_query(query="operation",operation="StartJobRun,GetJobRun,
    // BatchStopJobRun",model_path="services/service-com-amazonaws-glue-awsglue/
    // smithy-model/com-amazonaws-glue-awsglue.json")`).
    RunAJob {
        starts: "glue:startJobRun",
        job_id: "JobRunId",
        describe: "GetJobRun",
        describe_member: "RunId",
        status_at: &["JobRun", "JobRunState"],
        // `GetJobRun` answers `{"JobRun": …}` and this integration reports the RUN
        // itself: the task's result carries the run's own members (`Id`, `JobName`,
        // `JobRunState`, `ErrorMessage`, the instants and the capacity) at its top
        // level, which is what a `ResultPath` of such a task addresses and what the
        // failure's cause carries for a run that ended without doing its work.
        reports_at: &["JobRun"],
        succeeded: &["SUCCEEDED"],
        // The `JobRunState` values a run never moves out of again; `STARTING`,
        // `RUNNING`, `STOPPING` and `WAITING` each still owe a further state.
        failed: &["STOPPED", "FAILED", "TIMEOUT", "ERROR", "EXPIRED"],
        json_text_at: &[],
        stop: "BatchStopJobRun",
        stop_member: "JobRunIds",
        stops_in_batch: true,
        names_job_with: &["JobName"],
        // `GetJobRun` carries the run's instants inside the `JobRun` it answers with,
        // which is not a member of the report's top level.
        instants_at: &[],
        // The report of a job run carries no document of the run's own.
        redacted_in_cause: &[],
        // Both of those calls are taken on the JOB the run belongs to, which the
        // starting request named ([`TARGET_RESOURCES`]).
        acts_on: Some(JobTarget::Started("glue:startJobRun")),
    },
];
