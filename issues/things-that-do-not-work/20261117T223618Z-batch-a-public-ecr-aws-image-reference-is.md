Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.deploy-job-queue-via-cloudformation-accepts-and-runs-job
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.deploy-job-queue-via-cloudformation-accepts-and-runs-job|run#32|AssertionError|assert final["status"] != "FAILED", f"job failed instead of running: {final.get('statusReason')}"
Issue-Commit: e311c11a5225a75aa165947dc04df865f5f68358

# batch: a public.ecr.aws image reference is resolved against the caller's PRIVATE ECR registry, so a job submitted to a CloudFormation-deployed queue never leaves FAILED/CannotPullContainerError

> _Note: this issue was identified against the code base at commit `e311c11a5225a75aa165947dc04df865f5f68358`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
One net-new behavioral root cause, grounded in the single failing check of the `batch` service group that is neither ALREADY FILED nor OWNED by an open issue.

Related open issues, named for the fixer (neither carries this check's replay key, so neither covers it): 20261116T211845Z-batch-a-cloudformation-deployed-batch-estate-can-never.md binds only batch.deploy-job-queue-via-cloudformation-processes-job, and 20261116T095051Z-batch-ecs-logs-an-amazon-ecr-public-image.md is scoped to the batch__ecs__logs group. Both record the same misresolution; one fix should close all three, but the check filed here must be re-replayed to confirm it.

Cause 1 of 1: an Amazon ECR Public image reference (registry host `public.ecr.aws`) is looked up as a private repository in the caller's own account, so no Batch job whose container image is a public image can ever be placed and run.

AWS: Smithy com.amazonaws.batch#ContainerProperties.image documents the accepted reference forms explicitly and separately — "Images in Amazon ECR Public repositories use the full registry/repository[:tag] or registry/repository[@digest] naming conventions. For example, public.ecr.aws/<registry_alias>/<my-web-app>:<latest>" versus "Images in Amazon ECR repositories use the full registry and repository URI (for example, 123456789012.dkr.ecr.<region-name>.amazonaws.com/<repository-name>)", plus Docker Hub single/organization-qualified names and other domain-qualified hosts. A public.ecr.aws/... reference therefore resolves in the Amazon ECR Public Gallery and is never a repository in the caller's account. A FARGATE job whose image is an EXISTING public tag pulls it, runs its command and progresses to RUNNING and then SUCCEEDED with container.exitCode 0. Only a reference that genuinely does not resolve fails the attempt, and then the status reason begins with the fixed text "CannotPullContainerError: " followed by variable parts taken from the request and from the registry actually consulted (the reference from containerProperties.image plus that registry's own not-found wording); the caller's account id is never named for a public reference.

Emulator: the container image resolver on the Batch placement path treats the registry host `public.ecr.aws` as the account's private ECR registry and looks the remaining path up as a private repository name in the local account. The job is driven to terminal status FAILED with statusReason (and attempts[].container.reason) "CannotPullContainerError: failed to resolve ref <image ref>: not found: The repository with name '<path after public.ecr.aws/>' does not exist in the registry with id '<the emulator's account id>'" — the private-registry RepositoryNotFound sentence, naming the caller's account as the registry that should have held a PUBLIC image. Fixed text of the diverging reason: "CannotPullContainerError: failed to resolve ref", ": not found: The repository with name", "does not exist in the registry with id". Variable parts: the image reference and repository path taken from containerProperties.image, and the emulator's own account id (a per-run value, not itself the requirement). In this check the whole estate — IAM role, MANAGED/FARGATE compute environment and ENABLED job queue deployed by CloudFormation to CREATE_COMPLETE, queue VALID, RegisterJobDefinition accepted, SubmitJob accepted, and the job even tracked in GetJobQueueSnapshot.frontOfQueue — is healthy; only the image pull is wrong, so the deploy looks correct and the job still never runs.

Why: public.ecr.aws is the registry AWS's own Batch documentation and tutorials use for example images. A client that deploys a Batch estate (by CloudFormation or API) and submits a job on a public image never observes RUNNING or SUCCEEDED — the control plane passes end to end while the data plane silently cannot execute anything. The message also misdirects the operator: it asserts a repository is absent from THEIR private registry and prints their account id, so they will create a private repository that was never involved, and tooling that distinguishes a genuinely bad tag from a resolver bug by matching the reason text cannot, because both produce the same sentence.

Fix: the image-reference resolver used by the Batch/ECS container placement path. A reference whose registry host is a public registry (public.ecr.aws, Docker Hub short and organization-qualified names, and other domain-qualified hosts, all of which ContainerProperties.image documents as valid) must be resolved against that registry rather than rewritten into a private-registry lookup in the caller's account; only a genuinely unresolvable reference may fail the attempt, and its CannotPullContainerError text must name the registry actually consulted instead of the caller's account id.

Traces: batch.deploy-job-queue-via-cloudformation-accepts-and-runs-job.

## Evidence
Behavioral replay (simparity-behavioral-aws-v1) against this checkout.

[mismatch] batch.deploy-job-queue-via-cloudformation-accepts-and-runs-job
check_batch_deploy_job_queue_via_cloudformation_accepts_and_runs_job.py, line 197, in run:
  assert final["status"] != "FAILED", f"job failed instead of running: {final.get('statusReason')}"
AssertionError: job failed instead of running: CannotPullContainerError: failed to resolve ref public.ecr.aws/docker/library/busybox:1.36: not found: The repository with name 'docker/library/busybox' does not exist in the registry with id '<account-id>'

In that message, "CannotPullContainerError: failed to resolve ref", ": not found: The repository with name" and "does not exist in the registry with id" are the emulator's fixed text; "public.ecr.aws/docker/library/busybox:1.36" and "docker/library/busybox" are taken from the request's containerProperties.image, and the account id is the emulator's own per-run account value (not part of the requirement).

Expected (AWS): with the CloudFormation-deployed MANAGED/FARGATE compute environment and ENABLED job queue at CREATE_COMPLETE/VALID, a job definition whose containerProperties.image is an EXISTING public tag (public.ecr.aws/docker/library/busybox:1.36) is submitted, appears in GetJobQueueSnapshot.frontOfQueue, and DescribeJobs settles on status RUNNING and then SUCCEEDED with container.exitCode 0. No CannotPullContainerError is produced for an existing public image.
Actual (emulator): the stack, queue, job definition and SubmitJob all succeed and the job is tracked in frontOfQueue, but DescribeJobs settles on status FAILED carrying the private-registry not-found sentence above, so the container command never runs.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Create/resolve a VPC with a usable subnet and a security group.
3. CreateStack with a template declaring an AWS::IAM::Role for Batch, an AWS::Batch::ComputeEnvironment Type MANAGED with ComputeResources Type FARGATE referencing that subnet/security group, and an AWS::Batch::JobQueue Priority 1, State ENABLED, ComputeEnvironmentOrder referencing it (Capabilities CAPABILITY_NAMED_IAM). Wait for CREATE_COMPLETE.
4. Poll `aws batch describe-job-queues --job-queues <jq-arn>` until status VALID.
5. `aws batch register-job-definition --job-definition-name <jd> --type container --container-properties '{"image":"public.ecr.aws/docker/library/busybox:1.36","command":["echo","hello-from-cfn-deployed-batch"],"resourceRequirements":[{"type":"VCPU","value":"1"},{"type":"MEMORY","value":"512"}]}'`.
6. `aws batch submit-job --job-queue <jq-arn> --job-definition <jd-arn> --job-name <name>`; confirm the job appears in `aws batch get-job-queue-snapshot --job-queue <jq-arn>` frontOfQueue.
7. Poll `aws batch describe-jobs --jobs <jobId>` until status is RUNNING, SUCCEEDED or FAILED.
   Observed: FAILED, statusReason "CannotPullContainerError: failed to resolve ref public.ecr.aws/docker/library/busybox:1.36: not found: The repository with name 'docker/library/busybox' does not exist in the registry with id '<the caller's account id>'".
   AWS: RUNNING then SUCCEEDED with container.exitCode 0.
8. Re-replay the check id in trace_ids to confirm the fix.
