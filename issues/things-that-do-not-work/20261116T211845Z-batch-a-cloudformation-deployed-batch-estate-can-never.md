Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.deploy-job-queue-via-cloudformation-processes-job
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.deploy-job-queue-via-cloudformation-processes-job|run#14|AssertionError|assert final["status"] == "SUCCEEDED", (
Issue-Commit: e9c9756b532fd8030baa41ef53e696f50564c9dd

# batch: a CloudFormation-deployed Batch estate can never run a job — the job definition's public.ecr.aws image is resolved against the caller's PRIVATE ECR registry, so SubmitJob ends FAILED with CannotPullContainerError "The repository with name '<path>' does not exist in the registry with id '<account>'"

> _Note: this issue was identified against the code base at commit `e9c9756b532fd8030baa41ef53e696f50564c9dd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitJob

## Description
One net-new behavioral root cause, grounded in the one failing check of the `batch` service group that is not already owned by an open issue.

Related open issue, named for the fixer: 20261116T095051Z-batch-ecs-logs-an-amazon-ecr-public-image.md records the same misresolution, but it is scoped to the `batch__ecs__logs` service group and its Issue-Keys bind only batch__ecs__logs.batch-fargate-job-runs-as-ecs-task-and-logs and batch__ecs__logs.batch-job-with-missing-image-tag-fails-and-logs-nothing. It does NOT carry the replay key of the check filed here, so that check is not covered by it and is reported separately; the fixer should expect one fix to close both.

Cause 1 of 1: an Amazon ECR Public image reference is looked up as a private repository in the caller's own account, so no Batch job on a public image can ever run.

AWS: Smithy com.amazonaws.batch#ContainerProperties.image documents the accepted reference forms explicitly and separately — "Images in Amazon ECR Public repositories use the full registry/repository[:tag] or registry/repository[@digest] naming conventions. For example, public.ecr.aws/<registry_alias>/<my-web-app>:<latest>" versus "Images in Amazon ECR repositories use the full registry and repository URI (for example, 123456789012.dkr.ecr.<region-name>.amazonaws.com/<repository-name>)". A public.ecr.aws/... reference therefore resolves in the Amazon ECR Public Gallery and is NOT a repository in the caller's account. A FARGATE job whose image is an EXISTING public tag pulls it, runs its command and reaches status SUCCEEDED with container.exitCode 0. Only a reference that genuinely does not resolve is FAILED, and then the attempt/status reason begins with the fixed text "CannotPullContainerError: " followed by variable parts taken from the request and the registry actually consulted (the image reference from containerProperties.image plus that registry's own not-found wording); the caller's account id is never named for a public reference.

Emulator: the image resolver treats the registry host `public.ecr.aws` as the account's private ECR registry and looks the remaining path up as a private repository name in the local account. The job is driven to status FAILED with statusReason (and attempts[].container.reason) "CannotPullContainerError: failed to resolve ref <image ref>: not found: The repository with name '<path after public.ecr.aws/>' does not exist in the registry with id '<the emulator's account id>'" — the private-registry RepositoryNotFound sentence, naming the caller's account as the registry that should have held a PUBLIC image. In the failing check the whole estate (IAM role + MANAGED/FARGATE compute environment + ENABLED job queue + FARGATE job definition) is deployed by CloudFormation and reaches CREATE_COMPLETE, the queue goes VALID/ENABLED and SubmitJob is accepted — only the container pull is wrong, so the deploy looks healthy and the job still never succeeds. (The fixed text of the diverging reason is "CannotPullContainerError: failed to resolve ref", ": not found: The repository with name", "does not exist in the registry with id"; the variable parts are the image reference and repository path from containerProperties.image and the emulator's own account id, which is a per-run value and not itself the defect.)

Why: public.ecr.aws is the registry AWS's own Batch documentation and tutorials use for example images, so a client that deploys a Batch estate through CloudFormation and submits a job against it never observes a terminal SUCCEEDED job — the control-plane half of the deploy passes and the data-plane half silently cannot execute. The failure also misdirects the operator: it asserts a repository is absent from THEIR private registry and prints their account id, so they will go create a private repository that was never involved, and tooling that tells a genuinely bad tag apart from a resolver bug by matching the reason text cannot, because both produce the same sentence.

Fix: the image-reference resolver on the Batch/ECS container placement path. A reference whose registry host is a public registry (public.ecr.aws, and likewise Docker Hub short/organization-qualified names and other domain-qualified hosts, all of which ContainerProperties.image documents as valid) must be resolved against that registry, not rewritten into a private-registry lookup in the caller's account; only a genuinely unresolvable reference may fail the attempt, and its CannotPullContainerError text must name the registry actually consulted instead of the caller's account id.

Traces: batch.deploy-job-queue-via-cloudformation-processes-job.

## Evidence
Behavioral replay (simparity-behavioral-aws-v1) against this checkout.

[mismatch] batch.deploy-job-queue-via-cloudformation-processes-job
check_batch_deploy_job_queue_via_cloudformation_processes_job.py, line 118, in run:
  assert final["status"] == "SUCCEEDED", ...
AssertionError: job deployed via CloudFormation never ran to completion: FAILED (CannotPullContainerError: failed to resolve ref public.ecr.aws/docker/library/busybox:1.36: not found: The repository with name 'docker/library/busybox' does not exist in the registry with id '<account-id>')

In that message, "CannotPullContainerError: failed to resolve ref", ": not found: The repository with name" and "does not exist in the registry with id" are the emulator's fixed text; "public.ecr.aws/docker/library/busybox:1.36" and "docker/library/busybox" come from the request's containerProperties.image, and the account id is the emulator's own per-run account value (not part of the requirement).

Expected (AWS): the CloudFormation stack (AWS::IAM::Role + AWS::Batch::ComputeEnvironment MANAGED/FARGATE + AWS::Batch::JobQueue ENABLED + AWS::Batch::JobDefinition with PlatformCapabilities [FARGATE] and an EXISTING public.ecr.aws tag) reaches CREATE_COMPLETE; DescribeJobQueues reports status VALID and state ENABLED; SubmitJob is accepted and DescribeJobs settles on status SUCCEEDED with container.exitCode 0. No CannotPullContainerError is produced for an existing public image.
Actual (emulator): the stack reaches CREATE_COMPLETE and the queue reaches VALID/ENABLED, SubmitJob is accepted, but DescribeJobs settles on status FAILED with the private-registry not-found sentence above, so the job never runs its command.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Create a VPC with a public subnet, an internet gateway with a default route, and use the VPC's default security group.
3. CreateStack with a template declaring: AWS::IAM::Role trusting ecs-tasks.amazonaws.com with AmazonECSTaskExecutionRolePolicy; AWS::Batch::ComputeEnvironment Type MANAGED, ComputeResources Type FARGATE with MaxvCpus 2 and that subnet/security group; AWS::Batch::JobQueue Priority 1, State ENABLED, ComputeEnvironmentOrder referencing the compute environment; AWS::Batch::JobDefinition Type container, PlatformCapabilities [FARGATE], ContainerProperties.Image "public.ecr.aws/docker/library/busybox:1.36" (an EXISTING public tag), Command ["echo","hello-from-cfn-batch"], ExecutionRoleArn from the role, ResourceRequirements VCPU 0.25 / MEMORY 512, NetworkConfiguration AssignPublicIp ENABLED. Capabilities CAPABILITY_NAMED_IAM. Wait for CREATE_COMPLETE.
4. Poll `aws batch describe-job-queues --job-queues <jq-name>` until status VALID; confirm state ENABLED.
5. `aws batch submit-job --job-queue <jq-name> --job-definition <jd-name> --job-name <name>`.
6. Poll `aws batch describe-jobs --jobs <jobId>` to a terminal status.
   Observed: status FAILED, statusReason "CannotPullContainerError: failed to resolve ref public.ecr.aws/docker/library/busybox:1.36: not found: The repository with name 'docker/library/busybox' does not exist in the registry with id '<the caller's account id>'".
   AWS: status SUCCEEDED with container.exitCode 0.
7. Re-replay the check id in trace_ids to confirm the fix.
