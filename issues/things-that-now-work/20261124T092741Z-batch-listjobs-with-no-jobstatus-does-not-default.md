Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.array-job-children-receive-distinct-array-index
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.array-job-children-receive-distinct-array-index|run#17|AssertionError|assert unfiltered == [], (
Issue-Commit: d08982fa92d616a1168765b1de7f11f99f6aaf82

# batch: ListJobs with no jobStatus does not default to RUNNING when listing by arrayJobId, so terminal children are returned

> _Note: this issue was identified against the code base at commit `d08982fa92d616a1168765b1de7f11f99f6aaf82`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListJobs

## Description
One net-new root cause in the AWS Batch emulator, grounded in a failing behavioral check that the replay report marks neither ALREADY FILED nor OWNED.

Cause 1 of 1: the implicit RUNNING status filter is skipped for a listing selected by arrayJobId

AWS: Smithy (com.amazonaws.batch#ListJobs, POST /v1/listjobs, 200 with required jobSummaryList) documents ListJobsRequest$jobStatus as "The job status used to filter jobs in the specified queue. If the filters parameter is specified, the jobStatus parameter is ignored and jobs with any status are returned. The exception is the SHARE_IDENTIFIER filter... If you don't specify a status, only RUNNING jobs are returned." The member doc adds the array-parent note ("Array job parents are updated to PENDING ... To view these jobs, filter by PENDING status until all child jobs reach a terminal state"), i.e. the status default is NOT lifted for array listings — a caller must name the status it wants. The recorded behavior in the blessed check confirms it: with arrayJobId naming a size-2 array whose two children are SUCCEEDED and FAILED, ListJobs(arrayJobId=<parent>) with no jobStatus returns HTTP 200 with jobSummaryList == [] (no child is RUNNING), while ListJobs(arrayJobId=<parent>, jobStatus='SUCCEEDED') and jobStatus='FAILED' each return exactly one child. This is a 200-vs-200 content divergence; no error code or message is involved.
Emulator: the implicit RUNNING default is applied ONLY when the request selected by jobQueue. For a listing selected by arrayJobId (or multiNodeJobId) with no jobStatus, no status narrowing is applied at all and EVERY child is returned whatever its status. The replay shows ListJobs(arrayJobId=<parent>) returning both terminal children, e.g. [('<parent-id>:0', 'SUCCEEDED'), ('<parent-id>:1', 'FAILED')] where AWS returns an empty list; the ids and statuses are per-run values, the requirement is that children not in RUNNING are absent.
Why: a client that enumerates "what is currently running in this array" — the documented meaning of an unfiltered ListJobs — is handed terminal children and concludes work is still in flight; progress/completion polling loops built on the unfiltered listing never see the array drain, and dashboards double-count children that already exited. The emulator is also inconsistent with its own queue listing, so the same job appears or disappears depending on which selector was used.
Fix: the status-narrowing branch of the ListJobs handler (services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/list_jobs.rs, lines 365-384: the match on (input.job_status, selected_by_queue) whose (None, false) arm is a deliberate no-op). The implicit RUNNING default must apply to every selector, not just jobQueue; an explicitly named jobStatus keeps selecting as it does today, and the filters-present path (status_filter_applies == false, except SHARE_IDENTIFIER) is unaffected.
Traces: batch.array-job-children-receive-distinct-array-index (AssertionError: "ListJobs with no jobStatus defaults to RUNNING only, and both children are terminal, so the array must list as empty").

Related but not covering this: the resolved issue 20261022T221856Z-batch-deletecomputeenvironment-deletejobqueue-createquotashare-describequotashare-answer-501-and.md (Cause 5) changed the same handler's filter RESOLUTION (unknown jobQueue/arrayJobId must be an empty page rather than a refusal). That fix landed and is not regressed here — this call is accepted with 200; the defect is the status default applied to the selected set.

## Evidence
From the simparity-behavioral-aws-v1 replay (batch 223/251 passed):

[mismatch] batch.array-job-children-receive-distinct-array-index
check_batch_array_job_children_receive_distinct_array_index.py, line 500, in run:
    unfiltered = bat.list_jobs(arrayJobId=parent_id).get("jobSummaryList", [])
    assert unfiltered == [], ...
AssertionError: ListJobs with no jobStatus defaults to RUNNING only, and both children are terminal, so the array must list as empty; got [('7b8140be-128b-45a7-854a-fd62adeab8fa:0', 'SUCCEEDED'), ('7b8140be-128b-45a7-854a-fd62adeab8fa:1', 'FAILED')]

The check reached this assertion only after the array job itself behaved correctly (parent FAILED, statusSummary aggregated, parent container carries no runtime fields), so the divergence is isolated to the unfiltered ListJobs listing.

Model facts used: smithy_query(model_path="services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json", operation="ListJobs") — input {jobQueue, arrayJobId, multiNodeJobId, jobStatus(JobStatus enum), maxResults, nextToken, filters}, output requires jobSummaryList, errors ClientException(400)/ServerException(500), http POST /v1/listjobs code 200. smithy_query(query="doc", shape="ListJobsRequest", member="jobStatus") — "If you don't specify a status, only RUNNING jobs are returned." plus the array-parent note telling callers to filter by PENDING explicitly.

Source: services/service-com-amazonaws-batch-awsbatchv20160810/src/ops/list_jobs.rs:379-383 —
    match (input.job_status.as_deref(), selected_by_queue) {
        (Some(status), _) => jobs.retain(|job| job.status == status),
        (None, true)      => jobs.retain(|job| job.status == batch::JOB_RUNNING),
        (None, false)     => {}
    }
The (None, false) arm (an arrayJobId/multiNodeJobId listing with no jobStatus) applies no status narrowing; the module doc at lines 10-13 and 366-375 states this exemption as intentional.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite (check batch.array-job-children-receive-distinct-array-index).
2. Create a MANAGED/FARGATE compute environment and a job queue, and register a Fargate job definition whose command exits with $AWS_BATCH_JOB_ARRAY_INDEX.
3. aws batch submit-job --array-properties size=2 ... and wait for both children to reach a terminal state (child 0 SUCCEEDED, child 1 FAILED).
4. aws batch list-jobs --array-job-id <parentJobId>  (no --job-status)
   Expected (AWS): HTTP 200 with jobSummaryList == [] — no child is RUNNING.
   Actual (emulator): HTTP 200 listing both terminal children, with status SUCCEEDED and FAILED.
5. aws batch list-jobs --array-job-id <parentJobId> --job-status SUCCEEDED and ... --job-status FAILED each still return their one child, confirming the divergence is only in the no-jobStatus default.
