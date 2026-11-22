Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecr__ecs.ecsnrt-fargate-task-on-unpushed-tag-fails-pull-until-retagged
Issue-Key: ec2__ecr__ecs.ecsnrt-fargate-task-on-unpushed-tag-fails-pull-until-retagged|_poll#17|Timeout|time.sleep(interval_s)
Issue-Commit: ce4ee9e56b2f664f3ccf49966acace7716de13e8

# ec2__ecr__ecs: the unpushed-ECR-tag Fargate pull scenario never reaches a verdict — RunTask tasks never settle to STOPPED, so a missing tag is never represented as TaskFailedToStart with no imageDigest

> _Note: this issue was identified against the code base at commit `ce4ee9e56b2f664f3ccf49966acace7716de13e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
One net-new root cause, grounded in the failing check the suite lists under `### ec2__ecr__ecs` that is marked neither ALREADY FILED nor OWNED (the other two checks under that heading are OWNED by 20261119T170900Z-ec2-ecr-ecs-deletestack-on-an-ecs-on.md and 20261103T021956Z-ec2-ecr-ecs-ecs-fargate-ephemeral-storage-bounds-unenforced-services-are.md). Related open issue that does NOT carry this check's replay key: 20261120T151638Z-ecr-ecs-elbv2-a-fargate-task-whose-ecr.md (the same family of ECR-pull failures under a different service heading).

Cause 1 of 1: a Fargate task whose container image names an ECR tag that was never pushed does not settle, so the image-pull outcome is never observable: the scenario consumes its whole runner budget waiting for DescribeTasks to report lastStatus=STOPPED for the task and its control twin, and the check is cut off with no verdict.
AWS: recorded behavior only (the ECS model does not state pull semantics; Task.stopCode/Container.imageDigest/Task.startedAt are plain output members). In the recording: two RunTask calls on the same cluster with launchType=FARGATE and assignPublicIp=ENABLED, one task definition pointing at a pushed tag and one at a tag that was never pushed, both reach lastStatus=STOPPED; both report stopCode='TaskFailedToStart' (the image has no runnable entrypoint, so even a resolved pull stops at container start); the control's containers[].imageDigest is the digest PutImage assigned to the pushed manifest, while the unpushed-tag task reports NO containers[].imageDigest and no startedAt at all; and after ecr:PutImage re-tags that same manifest under the referenced tag, re-running the identical task definition resolves that digest. The repository name, tags, digest and task ARNs are request/state values; the requirement is the STOPPED settlement and the digest-present vs digest-absent distinction.
Emulator: the check never completes — the replay reports `Error: Check timed out after 60.0s`, i.e. DescribeTasks never satisfied "both tasks STOPPED" inside the budget (the check itself allows 600 s for the stop). No assertion about the pull was ever reached, so neither stopCode nor imageDigest could be compared. Hypothesis: a Fargate task launched by RunTask does not progress to STOPPED when its image cannot be resolved (and the pull failure is not simulated at all), which is the same stuck-data-plane behaviour already filed across the ECS suite.
Why: a client cannot tell a broken image reference from a healthy deployment. The canonical CI/CD diagnosis — run the task, read stopCode='TaskFailedToStart' and the absent containers[].imageDigest, fix the tag, re-run and see the digest resolve — is impossible, and any workflow or test that waits for a task to stop hangs instead of failing fast.
Fix: the ECS RunTask Fargate task lifecycle and its ECR image-resolution step — resolve the task definition's image reference against the ECR repository at launch, and when the tag does not exist stop the task (lastStatus=STOPPED, stopCode='TaskFailedToStart', no startedAt, no containers[].imageDigest) instead of leaving it pre-terminal; a resolved pull must record the manifest's digest on the container.
Traces: ec2__ecr__ecs.ecsnrt-fargate-task-on-unpushed-tag-fails-pull-until-retagged.

## Evidence
Suite: `ec2__ecr__ecs` 5/8 (62%), 3 failed — 2 OWNED, this one net-new.

Expected (AWS): after RunTask on both task definitions, DescribeTasks shows both tasks STOPPED; the pushed-tag control has stopCode='TaskFailedToStart' and containers[0].imageDigest == the digest PutImage assigned; the unpushed-tag task has stopCode='TaskFailedToStart', containers[0].imageDigest absent and startedAt absent; after PutImage re-tags the manifest, a re-run of the same definition resolves that digest.
Actual (mismatch verdict): the scenario reaches no verdict —
  Error: Check timed out after 60.0s
The bounded wait that did not complete is DescribeTasks polling for `all(t["lastStatus"] == "STOPPED")` (the check's own ceiling is 600 s). The 60 s figure is the runner's remaining budget for this check and is a per-run value, not the requirement.

## Steps to Reproduce
1. CreateRepository in ECR; PutImage a manifest under tag `dev_build_service_main` and record the returned digest. Do NOT push `dev_build_service_latest`.
2. CreateRole trusting ecs-tasks.amazonaws.com with AmazonECSTaskExecutionRolePolicy attached; CreateCluster; create a VPC/public subnet.
3. RegisterTaskDefinition twice (networkMode=awsvpc, requiresCompatibilities=[FARGATE], cpu=256, memory=512, executionRoleArn set): one container image <repositoryUri>:dev_build_service_main, one <repositoryUri>:dev_build_service_latest.
4. RunTask both with launchType=FARGATE and awsvpcConfiguration{subnets=[subnet], assignPublicIp=ENABLED}.
5. Poll DescribeTasks for lastStatus=STOPPED on both: AWS settles both; the emulator never does, so the scenario is cut off.
6. On AWS, compare stopCode / containers[].imageDigest / startedAt, then PutImage the same manifest under dev_build_service_latest and re-run the second definition to see the digest resolve.
7. Re-replay check ec2__ecr__ecs.ecsnrt-fargate-task-on-unpushed-tag-fails-pull-until-retagged to confirm.
