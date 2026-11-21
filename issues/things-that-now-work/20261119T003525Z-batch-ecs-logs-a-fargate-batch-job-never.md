Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecs__logs.batch-fargate-job-runs-as-ecs-task-and-logs batch__ecs__logs.batch-job-with-missing-image-tag-fails-and-logs-nothing
Issue-Key: batch__ecs__logs.batch-fargate-job-runs-as-ecs-task-and-logs|_poll_until#24|Timeout|time.sleep(interval_s)
Issue-Key: batch__ecs__logs.batch-job-with-missing-image-tag-fails-and-logs-nothing|run#20|AssertionError|assert bad_job["status"] == "FAILED", (
Issue-Commit: e822fa4ca12543e97642b0a71000bedf3ee319c6

# batch__ecs__logs: a Fargate Batch job never runs as an ECS task and delivers no logs, and a job whose image tag does not exist is reported SUCCEEDED instead of FAILED with CannotPullContainerError (2 causes)

> _Note: this issue was identified against the code base at commit `e822fa4ca12543e97642b0a71000bedf3ee319c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
Two net-new root causes in the Batch→ECS→CloudWatch Logs path. Related issues that do NOT cover these checks (the suite left both unowned): 20261116T095051Z-batch-ecs-the-ecs-task-a-placed-batch.md, 20261116T095051Z-batch-ecs-logs-an-amazon-ecr-public-image.md, and the resolved 20261107T153156Z-batch-ecs-logs-a-job-whose-image-tag.md (which reported the image-tag job as FAILED with a generic reason; it is now reported SUCCEEDED, a different wrong answer, and the failure here is a wrong terminal status rather than a refusal).

Cause 1 of 2: a SUCCEEDED Fargate Batch job is not observable as a stopped FARGATE ECS task in the compute environment's cluster, and its reported batch/ log stream holds no event.
AWS: a Batch job placed on a MANAGED FARGATE compute environment runs as an ECS task in that CE's backing cluster; DescribeJobs reports the task in attempts[].container.taskArn (Smithy: AttemptContainerDetail.taskArn — "The Amazon Resource Name (ARN) of the Amazon ECS task that's associated with the job attempt. Each container attempt receives a task ARN when they reach the STARTING status"), ecs:DescribeTasks resolves that task as a stopped FARGATE task with exit 0, and the container's marker line is delivered to the stream named by AttemptContainerDetail.logStreamName ("Each container attempt receives a log stream name when they reach the RUNNING status"). Per-line awslogs delivery for Batch is documented at https://docs.aws.amazon.com/batch/latest/userguide/using_awslogs.html.
Emulator: the check never reaches a verdict — it is killed by the runner at its 60 s budget while waiting for the job's observable ECS task and the container marker in its log stream. The job settles immediately with a synthesised exitCode 0 and the fixed reason "Essential container in task exited", but nothing it names is readable: no log event appears in the batch/ stream, so the e2e assertion can never be satisfied.
Why: a client correlating a Batch job with the ECS task that ran it (for task-level metrics, stop reasons, or ENI attribution) and then reading the container's output gets a job that claims success while producing neither a resolvable task nor a single log line. Any workflow that treats the log stream as the job's output is unusable.
Fix: the Batch job data-plane runner — placement of a job onto the MANAGED FARGATE compute environment's backing ECS cluster as a real task, registration of that task in ECS state so DescribeTasks resolves it, and the awslogs log-driver delivery path that writes the container's stdout into the named log group/stream.
Traces: batch__ecs__logs.batch-fargate-job-runs-as-ecs-task-and-logs.

Cause 2 of 2: a job whose container image tag does not exist is reported SUCCEEDED with exit 0 instead of FAILED with a CannotPullContainerError reason.
AWS: when the ECS agent cannot pull the job's image, the task stops before any process runs; the Batch job reaches JobStatus FAILED (Smithy JobStatus enum: SUBMITTED, PENDING, RUNNABLE, STARTING, RUNNING, SUCCEEDED, FAILED), the attempt's container.reason reports the pull failure (the CannotPullContainerError class — reason text of the form "CannotPullContainerError: ..." naming the unresolvable image reference from the request; Smithy: AttemptContainerDetail.reason is "A short (255 max characters) human-readable string to provide additional details for a running or stopped container"), and the job's awslogs stream either does not exist or exists and stays empty because no process produced output. Smithy also states AttemptContainerDetail.exitCode: "A non-zero exit code is considered failed."
Emulator: the job submitted against a nonexistent image tag is reported with status SUCCEEDED and statusReason "Essential container in task exited" — the check asserts "a job whose image tag does not exist reported SUCCEEDED: Essential container in task exited". The image reference is never resolved against the registry, so the pull failure is never detected and the fixed clean-exit reason is emitted regardless of whether the image exists.
Why: a client's negative path inverts — a typo'd or unpublished image tag is reported as a successful job run. Deployment gates, retry policies and alarms keyed on FAILED never fire, and the operator is told work completed that never started. The message a client would parse to diagnose the pull ("CannotPullContainerError") is absent entirely.
Fix: the image-resolution step of the Batch job runner (resolve the container definition's image reference against the registry before the attempt is marked terminal) and the attempt's failure projection — on an unresolvable image the job must go FAILED with the CannotPullContainerError reason naming the requested image reference, and the log stream must hold no events.
Traces: batch__ecs__logs.batch-job-with-missing-image-tag-fails-and-logs-nothing.

## Evidence
Cause 1 — expected: DescribeJobs reports SUCCEEDED with a taskArn that ecs:DescribeTasks resolves as a stopped FARGATE task (exit 0) in the CE's cluster, and the reported batch/ log stream holds the container's marker line. Actual: "Error: Check timed out after 60.0s" — the check is killed before a verdict; the job settles with a synthesised exitCode 0 / reason "Essential container in task exited" and no log event is ever delivered to the stream it names.

Cause 2 — expected: status FAILED, attempts[].container.reason of the CannotPullContainerError class naming the unresolvable image reference from the request, and an awslogs stream that exists but holds no events, while a control job differing only in the image tag delivers its output. Actual: AssertionError "a job whose image tag does not exist reported SUCCEEDED: Essential container in task exited" — status SUCCEEDED, exit 0, and the fixed clean-exit reason text. "Essential container in task exited" is the emulator's fixed string emitted for every job; the required message's fixed part is the CannotPullContainerError class and its variable part is the image reference taken from the job definition in the request.

## Steps to Reproduce
1. Create a MANAGED FARGATE compute environment and a job queue bound to it; wait for VALID. Create a fresh CloudWatch Logs log group and confirm it has no streams.
2. Cause 1: RegisterJobDefinition with a container that prints a marker and an awslogs logConfiguration pointing at that log group; SubmitJob and poll DescribeJobs to SUCCEEDED. Take attempts[].container.taskArn and call ecs:DescribeTasks against the CE's cluster, then read attempts[].container.logStreamName with FilterLogEvents — neither the task nor any log event is resolvable.
3. Cause 2: RegisterJobDefinition with an image reference whose tag does not exist in the repository, same awslogs configuration; SubmitJob and poll DescribeJobs — the job reports SUCCEEDED with reason "Essential container in task exited" instead of FAILED with a CannotPullContainerError reason.
4. Submit a control job differing only in a valid image tag to confirm the two paths are indistinguishable in the emulator.
