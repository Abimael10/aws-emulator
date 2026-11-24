Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__logs.batch-array-child-index-drives-distinct-stdout
Issue-Key: batch__logs.batch-array-child-index-drives-distinct-stdout|_describe_job#8|Timeout|return js[0] if js else None
Issue-Commit: 8d18aae292383ed56856a9a0bfca586aad64bbda

# batch__logs: array-job children never execute, so AWS_BATCH_JOB_ARRAY_INDEX is never injected and no per-child log stream is written (1 cause)

> _Note: this issue was identified against the code base at commit `8d18aae292383ed56856a9a0bfca586aad64bbda`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
1 net-new root cause.

Cause 1 of 1: a size-N array job submitted to a MANAGED/FARGATE queue never advances its children to a terminal SUCCEEDED state, so nothing downstream of child execution ever happens: no container is run, AWS_BATCH_JOB_ARRAY_INDEX is never injected into a child's process environment, and no child ever receives a CloudWatch Logs stream to write to. Failing check id: batch__logs.batch-array-child-index-drives-distinct-stdout.

AWS: SubmitJob with arrayProperties.size = N creates one parent job and N children. Smithy (com.amazonaws.batch, AWS Batch v2016-08-10): ArrayProperties.size is the array size; ArrayPropertiesDetail.statusSummary is "A summary of the number of array job children in each available job status. This parameter is returned for parent array jobs", and ArrayPropertiesDetail.index is "The job index within the array that's associated with this job. This parameter is returned for array job children". Each child is an independent container execution: ContainerDetail.exitCode is "The exit code returned upon completion" and ContainerDetail.logStreamName is "The name of the Amazon CloudWatch Logs log stream that's associated with the container. The log group for Batch jobs is /aws/batch/job. Each container attempt receives a log stream name when they reach the RUNNING status." AWS Batch documents that it sets the reserved AWS_BATCH_JOB_ARRAY_INDEX environment variable in each child container to that child's zero-based index (the ContainerDetail.environment doc states the AWS_BATCH prefix "is reserved for variables that Batch sets"). So for a size-3 array job AWS converges to: parent arrayProperties.statusSummary SUCCEEDED == 3 and FAILED == 0; ListJobs(arrayJobId=parent) enumerates 3 children; DescribeJobs on those children reports arrayProperties.index exactly {0,1,2}, container.exitCode 0 for each, and three DISTINCT container.logStreamName values; and each child's own stream holds the one line its `echo IDX=$AWS_BATCH_JOB_ARRAY_INDEX` wrote, with the printed integer equal to that same child's reported arrayProperties.index.

Emulator: the children never reach SUCCEEDED. The check polls DescribeJobs on the parent for statusSummary.SUCCEEDED == size and never observes it — the poll never converges and the check is cut off ("Check timed out after 60.0s") before it can even get to the log assertions. The emulator's own view of the same scenario in this run is visible on the sibling array check in this report, batch.array-job-children-receive-distinct-array-index, where the parent settles with statusSummary {'SUCCEEDED': 0, 'STARTING': 0, 'FAILED': 2, 'RUNNABLE': 0, 'RUNNING': 0, 'SUBMITTED': 0, 'PENDING': 0}: children are driven straight to FAILED without ever being run. Because no container process is ever started, the index is never injected and no stream named for a child attempt is ever created, so the "each container's printed index equals the API's index for that child" invariant is unreachable rather than merely wrong.

Why: a client that fans work out with an array job gets a parent that never reports success and children that never do their work. Shard-per-index workloads (the standard Batch array pattern, where each child keys its input/output off AWS_BATCH_JOB_ARRAY_INDEX) silently produce nothing; a caller waiting on statusSummary.SUCCEEDED == size hangs forever, and a caller waiting on any terminal state sees an unexplained all-FAILED array. Any log-based verification or diagnosis path is also dead, since container.logStreamName is never populated and /aws/batch/job holds no child stream.

Fix: the SubmitJob array-job expansion and the job data plane that advances a placed child through STARTING/RUNNING to a terminal state — the child must actually be executed with the reserved AWS_BATCH_JOB_ARRAY_INDEX set to the child's own arrayProperties.index, must get its own awslogs stream, and the parent's ArrayPropertiesDetail.statusSummary aggregation must be driven from the children's real outcomes. Related open issues describe adjacent symptoms of the same data plane but do not carry this check's replay key: 20261104T041055Z-batch-logs-array-job-submission-is-unimplemented-and.md (array submission unimplemented, container jobs never run) and 20261107T153156Z-batch-logs-a-placed-batch-job-ends-failed.md (array children all fail with no reason). Neither covers this check.

Traces: batch__logs.batch-array-child-index-drives-distinct-stdout (primary), corroborated by batch.array-job-children-receive-distinct-array-index in the same run.

## Evidence
Expected (AWS, size-3 array job of `sh -c 'echo IDX=$AWS_BATCH_JOB_ARRAY_INDEX'`):
- DescribeJobs(parent).arrayProperties.statusSummary converges to SUCCEEDED=3, FAILED=0
- ListJobs(arrayJobId=parent) -> 3 child jobIds
- DescribeJobs(children): arrayProperties.index == {0,1,2}; container.exitCode == [0,0,0]; container.logStreamName set and all 3 distinct
- each child's log stream holds exactly one "IDX=<n>" line, <n> a digit string, and <n> == that child's arrayProperties.index; the three printed values are exactly {0,1,2}

Actual (emulator):
- the poll for statusSummary.SUCCEEDED == 3 never converges; check aborted: "Error: Check timed out after 60.0s"
- same-run corroboration from the sibling array check: parent arrayProperties.statusSummary = {'SUCCEEDED': 0, 'STARTING': 0, 'FAILED': 2, 'RUNNABLE': 0, 'RUNNING': 0, 'SUBMITTED': 0, 'PENDING': 0} — children go to FAILED without being executed
- consequently no container runs, AWS_BATCH_JOB_ARRAY_INDEX is never injected, and no per-child CloudWatch Logs stream exists under /aws/batch/job

Note on values: the array size (3), the queue/compute-environment names and the generated job ids are the check's inputs and per-run values, not requirements. The requirements are: SUCCEEDED == size and FAILED == 0 for a parent whose children all exit 0; one child per index 0..size-1; one distinct logStreamName per child; and the index printed by a child's container equal to that child's reported arrayProperties.index.

## Steps to Reproduce
1. Create a MANAGED FARGATE compute environment (maxVcpus 4) in the shared VPC, and a job queue associated with it; wait for both VALID.
2. Register a Fargate job definition whose command is ["sh", "-c", "echo IDX=$AWS_BATCH_JOB_ARRAY_INDEX"], with an awslogs logConfiguration pointing at a created log group and 0.25 vCPU resourceRequirements.
3. SubmitJob against that queue/definition with arrayProperties={"size": 3}.
4. Poll DescribeJobs(jobs=[parentId]) for arrayProperties.statusSummary.SUCCEEDED == 3. AWS converges; the emulator never does (children settle FAILED, SUCCEEDED stays 0).
5. ListJobs(arrayJobId=parentId), then DescribeJobs on the children: check arrayProperties.index == {0,1,2}, container.exitCode == 0 for each, and three distinct container.logStreamName values.
6. GetLogEvents on each child's stream and compare the printed IDX=<n> value against that same child's arrayProperties.index.
