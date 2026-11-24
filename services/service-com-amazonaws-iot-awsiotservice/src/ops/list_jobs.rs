// SPDX-License-Identifier: CC0-1.0

//! @op ListJobs
//!
//! The account's JOBS, as the shared jobs store holds them
//! (`service_common::iot_jobs`) — the one store every creator of an IoT job records one
//! in — each projected onto the `JobSummary` this model declares and narrowed by the
//! filters the request carries.
//!
//! What a job's TARGETS have made of it (its `JobStatus`, when it completed, whether it
//! is still concurrent) is counted from the shared job EXECUTIONS by
//! [`crate::state::job_progress`], the same projection `DescribeJob` reports a job
//! through, so this listing and a reader of one job cannot disagree about a job.
//!
//! ---
//!
//! # Modeled contract for `ListJobs`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json")
//! ```
//!
//! Lists jobs. Requires permission to access the ListJobs action.
//!
//! ## Input
//!
//! * `maxResults` (LaserMaxResults, range 1..250) — The maximum number of results to return
//!   per request.
//! * `namespaceId` (NamespaceId, pattern ^[a-zA-Z0-9_-]+$) — The namespace used to indicate
//!   that a job is a customer-managed job. When you specify a value for this parameter,
//!   Amazon Web Services IoT Core sends jobs notifications to MQTT topics that contain the
//!   value in the following format.
//!   $aws/things/THING_NAME/jobs/JOB_ID/notify-namespace-NAMESPACE_ID/ The namespaceId
//!   feature is only supported by IoT Greengrass at this time. For more information, see
//!   Setting up IoT Greengrass core devices.
//! * `nextToken` (NextToken) — The token to retrieve the next set of results.
//! * `status` (JobStatus, one of: CANCELED | COMPLETED | DELETION_IN_PROGRESS | IN_PROGRESS |
//!   SCHEDULED) — An optional filter that lets you search for jobs that have the specified
//!   status.
//! * `targetSelection` (TargetSelection, one of: CONTINUOUS | SNAPSHOT) — Specifies whether
//!   the job will continue to run (CONTINUOUS), or will be complete after all those things
//!   specified as targets have completed the job (SNAPSHOT). If continuous, the job may also
//!   be run on a thing when a change is detected in a target. For example, a job will run on
//!   a thing when the thing is added to a target group, even after the job was completed by
//!   all things originally in the group. We recommend that you use continuous jobs instead of
//!   snapshot jobs for dynamic thing group targets. By using continuous jobs, devices that
//!   join the group receive the job execution even after the job has been created.
//! * `thingGroupId` (ThingGroupId, len 1..128, pattern ^[a-zA-Z0-9\-]+$) — A filter that
//!   limits the returned jobs to those for the specified group.
//! * `thingGroupName` (ThingGroupName, len 1..128, pattern ^[a-zA-Z0-9:_-]+$) — A filter that
//!   limits the returned jobs to those for the specified group.
//!
//! ## Output
//!
//! * `jobs` (JobSummaryList) — A list of jobs.
//! * `nextToken` (NextToken) — The token for the next set of results, or null if there are no
//!   additional results.
//!
//! ## Errors
//!
//! * `InvalidRequestException` (HTTP 400) — The request is not valid.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource does not exist.
//! * `ServiceUnavailableException` (HTTP 503) — The service is temporarily unavailable.
//! * `ThrottlingException` (HTTP 400) — The rate exceeds the limit.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListJobsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iot-awsiotservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListJobsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iot#ListJobs")]
#[derive(Debug)]
pub enum ListJobsErrorModeled {
    InvalidRequestException(InvalidRequestException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
    ThrottlingException(ThrottlingException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_jobs::ListJobsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListJobsError = crate::scaffolded::Scaffolded<ListJobsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListJobsErrorModeled> for ListJobsError {
    fn from(modeled: ListJobsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListJobs` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListJobs<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListJobs<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListJobs<Eng>
{
    type Input = ListJobsInput;
    type Output = ListJobsOutput;
    type Error = ListJobsError;

    const NAME: &'static str = "ListJobs";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_JOBS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_JOBS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        _ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // The filters are resolved BEFORE any store is reached: a request whose
        // `nextToken` is not one this listing issued is refused whatever the store holds.
        let scope = listing_scope(&input);
        let resume_after = match input.next_token.as_deref() {
            Some(token) => Some(decode_next_token(token, &scope)?),
            None => None,
        };

        // `maxResults` is `range 1..250`, refused outside that before this handler runs.
        // A request that names none is answered with the LARGEST page the member admits,
        // not with everything stored: neither an absent nor an oversized `maxResults` may
        // turn one request into a response proportional to the whole store. The clamp also
        // keeps the page size inside the modelled range without depending on a validation
        // performed outside this module, and nothing is sized from the request.
        let page_size = input
            .max_results
            .unwrap_or(MAX_LASER_MAX_RESULTS)
            .clamp(1, MAX_LASER_MAX_RESULTS) as usize;

        let unavailable = || -> ListJobsError {
            ListJobsErrorModeled::ServiceUnavailableException(ServiceUnavailableException {
                message: Some("the IoT emulator has no state store attached".to_string()),
            })
            .into()
        };
        // The THING GROUP registry is this service's own store: it is what a group target
        // of a job is resolved against, both to report `JobSummary$thingGroupId` and to
        // answer the group filters.
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(unavailable());
        };
        // The jobs themselves live in the SHARED store every creator of an IoT job records
        // one in, so this listing reports a job whichever plane minted it.
        let Some(jobs) = crate::state::job_store(&*self.engine) else {
            return Err(unavailable());
        };
        // How far each job's targets have got with it is counted from the executions the
        // IoT Jobs data plane serves, so a listed job and the devices' own view of it are
        // one set of records read two ways.
        let Some(executions) = crate::state::job_execution_store(&*self.engine) else {
            return Err(unavailable());
        };

        let now = ::service_common::iot_job_executions::now_epoch_seconds();

        // The EXECUTIONS, in ONE pass and under their own lock, released before any other
        // store is reached: a timer the job or a device armed moves an execution to
        // `TIMED_OUT` by itself, so the transitions the clock has already made due are
        // recorded first — each was owed at its own deadline whether or not this call
        // happened — and then every job's executions are counted at once
        // ([`crate::state::tally_job_executions`]). Counting per job instead would walk
        // this whole store once per job, making one request cost the PRODUCT of the two
        // stores; this way it costs their sum, and the pass is bounded by the execution
        // store's own ceiling on how many records it holds at all.
        let tallies = {
            let mut pending = executions.lock();
            pending.time_out_due_executions_everywhere(now);
            crate::state::tally_job_executions(&pending)
        };

        // LOCK ORDER: this service's store first, the shared jobs store second, as
        // `crate::state::job_store` documents. The execution store is not held here.
        let store = state.lock();
        // "A filter that limits the returned jobs to those for the specified group"
        // (`thingGroupId`): the id names a group of the registry, and a job is "for" the
        // group its targets name — so the id is resolved to that group's NAME, which is
        // what a job's targets carry. An id the registry holds no group under names no
        // group, so it selects no job at all.
        let group_of_filtered_id: Option<String> = input.thing_group_id.as_deref().and_then(|id| {
            store
                .thing_groups
                .values()
                .find(|group| group.thing_group_id == id)
                .map(|group| group.thing_group_name.clone())
        });
        // Every group name a job has to target to be listed: the filtered NAME, and the
        // name the filtered ID resolves to. A request that carries neither filter requires
        // none, so every job's targets satisfy it.
        let mut required_group_names: Vec<&str> = Vec::new();
        if let Some(thing_group_name) = input.thing_group_name.as_deref() {
            required_group_names.push(thing_group_name);
        }
        if input.thing_group_id.is_some() {
            match group_of_filtered_id.as_deref() {
                Some(thing_group_name) => required_group_names.push(thing_group_name),
                // An id the registry holds no group under names no group, so no job can
                // be "for" it: the listing is empty, and there is nothing to continue
                // after.
                None => {
                    return Ok(ListJobsOutput {
                        jobs: Some(Vec::new()),
                        next_token: None,
                    });
                }
            }
        }

        let registry = jobs.lock();

        // The tally of a job no execution was counted for: every per-status counter at
        // zero, which is what a job whose targets hold no execution reports.
        let no_executions = crate::state::JobExecutionTally::default();
        let mut page: Vec<JobSummary> = Vec::new();
        // Where this call stopped LOOKING, which is where the next page resumes: the
        // position in the listing's order, so a page resumes where the last one ended even
        // if that job has since been deleted, and a page that stopped on the scan budget
        // does not make the next one re-examine what it already discarded.
        let mut last_examined: Option<&str> = None;
        let mut walked = 0usize;
        let mut stopped_early = false;
        // The resume point is looked up in the store's order rather than walked to, so a
        // later page does no work for the jobs before it.
        let mut rest = registry.jobs_after(resume_after.as_deref());
        for job in rest.by_ref() {
            walked += 1;
            last_examined = Some(job.job_id.as_str());
            // Which thing GROUPS this job is "for", and how far its targets have got with
            // it: each read once per job, from the targets the record carries and from the
            // tally counted above.
            let group_targets = thing_group_targets(job);
            let progress = crate::state::job_progress_from_tally(
                job,
                tallies.get(&job.job_id).unwrap_or(&no_executions),
            );
            // `targetSelection`: "Specifies whether the job will continue to run
            // (CONTINUOUS), or will be complete after all those things specified as
            // targets have completed the job (SNAPSHOT)" — the job's own, as its creator
            // recorded it, so a job whose creator named none is selected by no value.
            let selected = input
                .target_selection
                .as_deref()
                .is_none_or(|wanted| job.target_selection.as_deref() == Some(wanted))
                // A job is "for" a thing group when its targets name that group, so both
                // group filters are answered from the job's own targets.
                && required_group_names
                    .iter()
                    .all(|wanted| group_targets.iter().any(|target| target == wanted))
                // `status`: "An optional filter that lets you search for jobs that have
                // the specified status" — the status the job HAS, which is the one this
                // listing reports for it and not only the one its own record carries.
                && input
                    .status
                    .as_deref()
                    .is_none_or(|wanted| progress.status == wanted);

            if selected {
                page.push(JobSummary {
                    job_arn: Some(job.job_arn.clone()),
                    job_id: Some(job.job_id.clone()),
                    // "The ID of the thing group": the id the registry holds for the group
                    // this job targets. A job that targets no group, or one whose group the
                    // registry no longer holds, reports no id rather than an invented one.
                    thing_group_id: group_targets
                        .iter()
                        .find_map(|name| store.thing_groups.get(name.as_str()))
                        .map(|group| group.thing_group_id.clone()),
                    target_selection: job.target_selection.clone(),
                    status: Some(progress.status),
                    created_at: Some(::aws_smithy_types::DateTime::from_secs(job.created_at)),
                    last_updated_at: Some(::aws_smithy_types::DateTime::from_secs(
                        job.last_updated_at,
                    )),
                    completed_at: progress
                        .completed_at
                        .map(::aws_smithy_types::DateTime::from_secs),
                    is_concurrent: Some(progress.is_concurrent),
                });
            }

            // Two independent bounds hold per request: at most `page_size` summaries are
            // RETURNED, and at most `MAX_SCANNED_JOBS` jobs are EXAMINED, however
            // selective the filters are — so a filter almost nothing matches cannot make
            // one call walk the whole store. A page that stops on either comes back with a
            // token, which is how such a listing makes progress one bounded call at a time.
            if page.len() >= page_size || walked >= MAX_SCANNED_JOBS {
                stopped_early = true;
                break;
            }
        }

        // `nextToken` is "The token for the next set of results, or null if there are no
        // additional results": a token only where this call stopped with jobs still behind
        // it, so a caller that follows it always terminates.
        let next_token = match (stopped_early && rest.next().is_some(), last_examined) {
            (true, Some(job_id)) => Some(encode_next_token(&scope, job_id)),
            _ => None,
        };

        Ok(ListJobsOutput {
            // `JobSummaryList` carries no length constraint, so an account no job matches
            // answers with an EMPTY list rather than an absent member: "nothing matches"
            // is an answer, not a missing one.
            jobs: Some(page),
            next_token,
        })
    }
}

/// The largest page `LaserMaxResults` admits (`range 1..250`), which is also the page a
/// request that names no `maxResults` is answered with.
const MAX_LASER_MAX_RESULTS: i32 = 250;

/// The most jobs ONE request may EXAMINE, however selective its filters are.
///
/// A filtered page has to look at jobs it discards, so the walk — unlike the response — is
/// not bounded by the page size. This budget bounds it: when it runs out the page comes
/// back short, with a token at the last job EXAMINED, and the caller follows that token for
/// the rest. Set to ten pages' worth, so a selective filter still makes progress in one
/// call. The figure is the emulator's own resource guard, not an AWS-published quota.
const MAX_SCANNED_JOBS: usize = MAX_LASER_MAX_RESULTS as usize * 10;

/// The character that separates a token's scope from the position it resumes after. A
/// control character, so it cannot occur inside a `JobId`, a `JobStatus`, a
/// `TargetSelection`, a `ThingGroupName` or a `ThingGroupId`.
const TOKEN_SEPARATOR: char = '\u{1f}';

/// The thing GROUPS a job's targets name, in the order the job was created with them.
///
/// A target is an ARN, parsed the one way every operation that targets devices with a job
/// parses one ([`crate::state::parse_job_target`]); a target that names a thing, or that is
/// no IoT resource ARN at all, names no group.
fn thing_group_targets(job: &crate::state::RegisteredJob) -> Vec<String> {
    job.targets
        .iter()
        .filter_map(|target| match crate::state::parse_job_target(target) {
            Ok(crate::state::JobTarget::ThingGroup(group_name)) => Some(group_name),
            Ok(crate::state::JobTarget::Thing(_)) | Err(_) => None,
        })
        .collect()
}

/// The LISTING a continuation token belongs to: the filters, which decide which of the
/// account's jobs it walks and therefore what "the next set of results" means. A token
/// issued for one listing is refused by another.
///
/// `namespaceId` is not in the scope: the member is documented as the namespace a job's
/// notifications are published under — the MQTT topics IoT Core sends them to — and not as a
/// filter over the account's jobs, so it selects no subset of them and two requests that
/// differ only in it read ONE listing.
fn listing_scope(input: &ListJobsInput) -> String {
    [
        input.status.as_deref(),
        input.target_selection.as_deref(),
        input.thing_group_name.as_deref(),
        input.thing_group_id.as_deref(),
    ]
    .map(|filter| filter.unwrap_or_default())
    .join("\u{1e}")
}

/// The opaque `nextToken` that resumes `scope`'s listing after the job `job_id`.
///
/// The position, not a page number: it is the place in the listing's `jobId` order, so a
/// page resumes where the last one ended even if that job has since been deleted.
/// Hex-encoded so the token is opaque to the caller whatever the filters contain. The
/// encoding carries no integrity protection and claims none: a caller that reverses it can
/// name any resume point of the listing it is already reading, which selects a page of that
/// same listing and nothing else.
fn encode_next_token(scope: &str, job_id: &str) -> String {
    ::hex::encode(format!("{scope}{TOKEN_SEPARATOR}{job_id}"))
}

/// The position a `nextToken` resumes after, or `InvalidRequestException` (HTTP 400, "The
/// request is not valid") when the token is not one [`encode_next_token`] issued for THIS
/// listing.
///
/// Every step is fallible on purpose: the token is caller-supplied, so nothing read out of
/// it may panic the handler. A token issued for another set of filters is refused rather
/// than silently resuming a different listing from the middle.
fn decode_next_token(token: &str, scope: &str) -> Result<String, ListJobsError> {
    let invalid = || -> ListJobsError {
        ListJobsErrorModeled::InvalidRequestException(InvalidRequestException {
            message: Some("nextToken is not a continuation token of this listing".to_string()),
        })
        .into()
    };
    let bytes = ::hex::decode(token).map_err(|_| invalid())?;
    let decoded = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (token_scope, cursor) = decoded.split_once(TOKEN_SEPARATOR).ok_or_else(invalid)?;
    if token_scope != scope || cursor.is_empty() {
        return Err(invalid());
    }
    Ok(cursor.to_string())
}
