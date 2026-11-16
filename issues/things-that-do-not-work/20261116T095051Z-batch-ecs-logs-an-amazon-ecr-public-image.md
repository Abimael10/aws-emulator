Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecs__logs.batch-fargate-job-runs-as-ecs-task-and-logs batch__ecs__logs.batch-job-with-missing-image-tag-fails-and-logs-nothing
Issue-Key: batch__ecs__logs.batch-fargate-job-runs-as-ecs-task-and-logs|run#19|AssertionError|assert job["status"] == "SUCCEEDED", (
Issue-Key: batch__ecs__logs.batch-job-with-missing-image-tag-fails-and-logs-nothing|run#45|AssertionError|assert good_job["status"] == "SUCCEEDED", (
Issue-Commit: 47cc4ef4df4e13f0c580047fd2f3a66085f63081

# batch__ecs__logs: an Amazon ECR Public image reference (public.ecr.aws/...) is resolved against the account's PRIVATE registry, so every Batch Fargate job on a public image is FAILED with CannotPullContainerError "The repository with name '...' does not exist in the registry with id '<account>'"

> _Note: this issue was identified against the code base at commit `47cc4ef4df4e13f0c580047fd2f3a66085f63081`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
One net-new behavioral root cause, grounded in both failing checks of the batch__ecs__logs service group (0/2 passed). Related open issue, named for the fixer: 20261107T153156Z-batch-ecs-logs-a-job-whose-image-tag.md recorded that a bad image tag produced the generic "This emulator does not run containers" reason instead of CannotPullContainerError. The emulator now DOES produce a CannotPullContainerError, but it produces it for EVERY public.ecr.aws reference, including valid ones — that misresolution is the cause recorded here, and it is not the cause that issue describes.

Cause 1 of 1 — public.ecr.aws references are looked up in the caller's private ECR registry.
AWS: Smithy com.amazonaws.batch#ContainerProperties.image documents the accepted forms explicitly: "Images in Amazon ECR Public repositories use the full registry/repository[:tag] or registry/repository[@digest] naming conventions. For example, public.ecr.aws/<registry_alias>/<my-web-app>:<latest>", separately from "Images in Amazon ECR repositories use the full registry and repository URI (for example, 123456789012.dkr.ecr.<region-name>.amazonaws.com/<repository-name>)". A public.ecr.aws/... reference therefore resolves in the Amazon ECR Public Gallery — it is NOT a repository in the caller's account — and a Fargate job whose image is an existing public tag pulls it, runs, and reaches SUCCEEDED with container.exitCode 0, its ECS task STOPPED with exitCode 0 and its awslogs stream (batch/<container>/<ecs-task-id>) holding the container's stdout. Only an image reference that genuinely does not exist is failed, with the attempt's reason beginning "CannotPullContainerError: " and naming the reference from containerProperties.image (fixed text: the error class name "CannotPullContainerError"; variable part: the image reference from the request plus the registry's own not-found wording).
Emulator: the image resolver treats the registry host public.ecr.aws as the account's private ECR registry and looks the path up as a private repository name in the local account. Every public reference is reported missing, so the job is FAILED with statusReason and the attempt's container reason both "CannotPullContainerError: failed to resolve ref <image ref>: not found: The repository with name '<path after public.ecr.aws/>' does not exist in the registry with id '<the emulator account id>'" — i.e. the private-registry RepositoryNotFound sentence, naming the caller's account as the registry that should have held a PUBLIC image. A valid, existing public tag (the control leg of the missing-tag check, which differs from the failing leg only in the tag) fails identically, so the public-vs-missing differential collapses and no public-image job can ever run.
Why: public.ecr.aws is the registry AWS's own Batch documentation and nearly every Batch tutorial use for example images, so in practice no Batch job can execute at all against this emulator: it never reaches SUCCEEDED, never becomes a STOPPED ECS task with exit 0, and never writes an awslogs event. Worse, the failure is reported with a diagnosis that misdirects the operator — it says a repository is absent from THEIR private registry and names their account id, so they will go create a private repository that was never involved. Tooling that distinguishes a genuinely bad tag from a resolver bug by matching the reason text cannot: both legs carry the same sentence.
Fix: the image-reference resolver used by the Batch/ECS container placement path — a reference whose registry host is public.ecr.aws (and likewise other non-private registry hosts, e.g. Docker Hub short names) must be resolved against the public registry's catalog, not rewritten into a private-registry lookup in the caller's account; only a reference that is genuinely unresolvable may fail the attempt, and its CannotPullContainerError text must name the registry actually consulted rather than the caller's account id.
Traces: batch__ecs__logs.batch-fargate-job-runs-as-ecs-task-and-logs, batch__ecs__logs.batch-job-with-missing-image-tag-fails-and-logs-nothing.

## Evidence
Behavioral replay (simparity-behavioral-aws-v1) against this checkout.

[mismatch] batch__ecs__logs.batch-fargate-job-runs-as-ecs-task-and-logs
AssertionError: job did not succeed: status=FAILED statusReason=CannotPullContainerError: failed to resolve ref public.ecr.aws/docker/library/busybox:1.36: not found: The repository with name 'docker/library/busybox' does not exist in the registry with id '<account-id>' containerReason=<the same sentence>

[mismatch] batch__ecs__logs.batch-job-with-missing-image-tag-fails-and-logs-nothing
AssertionError: control job failed: CannotPullContainerError: failed to resolve ref public.ecr.aws/docker/library/busybox:1.36: not found: The repository with name 'docker/library/busybox' does not exist in the registry with id '<account-id>' <the same sentence again>

The image reference is the EXISTING public tag used as the control leg; the account id in the sentence is the emulator's own account, a per-run value. The defect is that a public.ecr.aws reference is answered with the private-registry "repository ... does not exist in the registry with id" sentence at all.

Expected (AWS): the job reaches SUCCEEDED with container.exitCode 0; ecs:DescribeTasks resolves container.taskArn in the CE's cluster with launchType FARGATE, lastStatus STOPPED and containers[0].exitCode 0; container.logStreamName starts with "batch/" and that CloudWatch Logs stream holds the container's marker line. Only the leg whose tag does not exist is FAILED, with a reason beginning "CannotPullContainerError" naming that reference, and an empty awslogs stream.
Actual (emulator): both legs FAILED with the private-registry not-found sentence; no job output and no log events at all.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Create a CloudWatch Logs log group and an ECS-tasks execution role with ecr:GetAuthorizationToken, ecr:BatchCheckLayerAvailability, ecr:GetDownloadUrlForLayer, ecr:BatchGetImage and logs:CreateLogStream, logs:PutLogEvents.
3. Create a MANAGED FARGATE compute environment (wait for VALID, note ecsClusterArn) and an ENABLED job queue in front of it.
4. Register a FARGATE job definition whose containerProperties.image is an existing Amazon ECR Public reference (e.g. public.ecr.aws/docker/library/busybox:<an existing tag>), command ["sh","-c","echo <marker>"], and an awslogs logConfiguration naming the log group.
5. aws batch submit-job against that queue/definition, then poll aws batch describe-jobs --jobs <jobId> to a terminal status.
   Observed: status FAILED, statusReason and attempts[0].container.reason = "CannotPullContainerError: failed to resolve ref <public ref>: not found: The repository with name '<path>' does not exist in the registry with id '<the caller's account id>'".
   AWS: status SUCCEEDED, container.exitCode 0, a STOPPED FARGATE ECS task in the CE's cluster with exitCode 0, and the batch/<container>/<task-id> log stream containing <marker>.
6. Repeat with a second definition identical except for a tag that does not exist: AWS fails only this one; the emulator fails both with the same sentence.
7. Re-replay the check ids in trace_ids.
