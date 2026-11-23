Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__s3.batch-array-24-shards-write-own-object-and-out-of-scope-put-denied batch__s3.batch-job-s3-write-outside-job-role-scope-is-denied batch__s3.batch-job-writes-s3-object-read-back-byte-exact
Issue-Key: batch__s3.batch-array-24-shards-write-own-object-and-out-of-scope-put-denied|_poll_until#5|Timeout|return last
Issue-Key: batch__s3.batch-job-s3-write-outside-job-role-scope-is-denied|_poll_until#16|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Key: batch__s3.batch-job-writes-s3-object-read-back-byte-exact|_poll_until#14|Timeout|return last
Issue-Commit: 2352e453041dca9d0b463884f1fb4473cbd545be

# batch__s3: a submitted Batch job never reaches a terminal status, so DescribeJobs never reports SUCCEEDED or FAILED and the container's S3 side effects never happen

> _Note: this issue was identified against the code base at commit `2352e453041dca9d0b463884f1fb4473cbd545be`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobs

## Description
Cause 1 of 1: a job accepted by SubmitJob is never driven to a terminal status. Every one of this service's three checks blocks in the same poll — DescribeJobs waiting for `status` in {SUCCEEDED, FAILED} — and none of them ever gets there. Failing check ids for this cause: batch__s3.batch-array-24-shards-write-own-object-and-out-of-scope-put-denied, batch__s3.batch-job-s3-write-outside-job-role-scope-is-denied, batch__s3.batch-job-writes-s3-object-read-back-byte-exact.

AWS: Smithy (service-com-amazonaws-batch-awsbatchv20160810): `SubmitJob` POST /v1/submitjob answers 200 with required `jobId` and `jobName` plus `jobArn`; `DescribeJobs` POST /v1/describejobs answers 200 with `jobs: JobDetailList`, each entry carrying `status` drawn from the JobStatus enum (SUBMITTED | PENDING | RUNNABLE | STARTING | RUNNING | SUCCEEDED | FAILED) and `container.exitCode` / `container.reason`. The model declares only ClientException (400) and ServerException (500) on both operations, so neither is modelled to leave a job suspended. Recorded behavior: after SubmitJob on an ENABLED Fargate queue, AWS places the job, RUNS its container under `jobRoleArn`, and DescribeJobs settles on exactly one of SUCCEEDED (container.exitCode 0) or FAILED; the container's own calls then show up as real effects — the copy destination object exists in S3 with ContentLength equal to the payload length and an ETag equal to the single-part MD5 of its bytes, an out-of-scope PutObject is refused by the job role, and each array child writes its own object. Those effects are the whole observable of this service group.

Emulator: the job is accepted (SubmitJob answers a jobId) but DescribeJobs keeps reporting a non-terminal status for the life of the check. batch__s3.batch-job-s3-write-outside-job-role-scope-is-denied surfaces it explicitly: `TimeoutError: batch job <job-id> status in ['FAILED', 'SUCCEEDED']: the run budget ended first; not graded` raised from the check's own `_await_job`/`_poll_until`; the other two checks exhaust the suite budget (`Check timed out after 60.0s`) in the same wait. No container ever runs, so no destination object, no per-shard object and no job-role denial is ever produced.

Why: a client cannot use AWS Batch at all. Every real caller polls DescribeJobs (or waits on the `JobComplete`-style loop the SDKs and the CLI implement) until the status is terminal; against this emulator that loop never exits, so the caller hangs instead of succeeding or failing. Anything downstream of the job — the object the container writes, the IAM denial the container earns, the array fan-out — is silently absent, so a test that asserts the job's effect sees an empty bucket rather than a verdict.

Fix: the Batch job data plane — the placement/dispatch path behind the SubmitJob handler and the job state machine that DescribeJobs reads, including the array-job child expansion. The state machine must advance a placed job through to SUCCEEDED or FAILED and set container.exitCode/container.reason accordingly; the container's credentials must be the job definition's jobRoleArn so its S3 calls are authorized (or denied) as that role.

Related open issue, which does NOT cover this: 20261106T090000Z-batch-s3-a-placed-job-s-container-is-never-run.md reports a batch__s3 job that REACHES FAILED with no container run; the failures here never reach a terminal status at all, so the job is not even observable as failed.

Traces: the three failing check ids listed above.

## Evidence
Expected (AWS, from the recording and the model): SubmitJob answers 200 with a jobId; DescribeJobs on that jobId eventually reports `status` == "SUCCEEDED" with `container.exitCode` == 0 (or "FAILED" with a container reason on the negative checks); the copy destination object exists with ContentLength equal to the uploaded payload length and ETag equal to the single-part MD5 of its bytes; the bucket holds exactly the source and destination keys; an out-of-scope PutObject by the job role is refused; each of the 24 array children writes its own object.

Actual (emulator): DescribeJobs never reports a terminal status.
- batch__s3.batch-job-s3-write-outside-job-role-scope-is-denied: `TimeoutError: batch job <job-id> status in ['FAILED', 'SUCCEEDED']: the run budget ended first; not graded` (raised in the check's _await_job -> _poll_until).
- batch__s3.batch-job-writes-s3-object-read-back-byte-exact: `Check timed out after 60.0s`, blocked in `_await_job(bat, job_id, {"SUCCEEDED", "FAILED"})`.
- batch__s3.batch-array-24-shards-write-own-object-and-out-of-scope-put-denied: `Check timed out after 60.0s`, blocked in the same terminal-status wait.
The job id, bucket name and object keys above are per-run values; the requirement is that DescribeJobs settles on a JobStatus terminal value and that the container's S3 writes/denials occur.

## Steps to Reproduce
1. Create an S3 bucket and PutObject a source key under src/.
2. Create an IAM job role allowing only s3:GetObject on arn:aws:s3:::<bucket>/src/* and s3:PutObject on arn:aws:s3:::<bucket>/dst/*, and an execution role able to pull the image and write the log stream.
3. Create a Fargate compute environment and job queue (maxVcpus 4) and wait for them to be VALID/ENABLED.
4. RegisterJobDefinition with the AWS CLI image and command ["s3","cp","s3://<bucket>/src/<key>","s3://<bucket>/dst/<key>","--region",<region>], jobRoleArn = the scoped role, retryStrategy attempts=2.
5. SubmitJob to that queue and poll DescribeJobs for `status` in {SUCCEEDED, FAILED}.
6. AWS settles on SUCCEEDED with container.exitCode 0 and the destination object present; the emulator never leaves a non-terminal status and the poll exhausts its budget.
7. Repeat with arrayProperties size 24 (each child writes its own object) and with a put outside the role's prefix (expected denial) — both block in the same wait.
