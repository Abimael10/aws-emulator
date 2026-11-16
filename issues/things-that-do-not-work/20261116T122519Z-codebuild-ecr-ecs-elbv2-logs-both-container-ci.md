Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Commit: 80ea869c5d431cbbd6e55b502825120813eb8704

# codebuild__ecr__ecs__elbv2__logs: both container CI-CD chain checks exhaust the 60s harness budget before any observable step, so no build outcome, image, rollout or ALB body is ever produced (1 cause)

> _Note: this issue was identified against the code base at commit `80ea869c5d431cbbd6e55b502825120813eb8704`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartBuild

## Description
Cause 1 of 1: the CodeBuild -> ECR -> ECS/Fargate -> ALB chain makes no observable progress. Both checks of this service now die with `Check timed out after 60.0s`, i.e. the chain does not even reach the first assertion that reads a build outcome; nothing downstream (the pushed image digest, the task definition pinned to it, the ECS rollout state, the ALB-served body, the circuit-breaker rollback) is reachable. Failing checks: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image and codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

AWS: Smithy: codebuild models the build lifecycle as `BuildPhaseType` {SUBMITTED, QUEUED, PROVISIONING, DOWNLOAD_SOURCE, INSTALL, PRE_BUILD, BUILD, POST_BUILD, UPLOAD_ARTIFACTS, FINALIZING, COMPLETED} and the outcome as `StatusType` {SUCCEEDED, FAILED, FAULT, TIMED_OUT, IN_PROGRESS, STOPPED}. `StartBuild` on a healthy project answers 200 with `build.buildStatus` 'IN_PROGRESS', and `BatchGetBuilds` then walks the build to a terminal `buildStatus` 'SUCCEEDED' with every `phases[].phaseStatus` 'SUCCEEDED'; the image the buildspec pushes becomes addressable in ECR by a `sha256:` digest, and an ECS service pinned to that digest behind an ALB target group reaches `deployments[0].rolloutState` 'COMPLETED' and serves the baked body, while a revision whose container never passes its health check reaches `rolloutState` 'FAILED' and is rolled back with the previous bytes still served. Beyond the status/enum vocabulary the model says nothing about timing; recorded behavior only for the latency — on AWS each of these steps returns within the check's polling windows.

Emulator: neither check produces any step verdict. The harness reports only `Check timed out after 60.0s` for both, with no HTTP status, error code or message captured — the chain is still blocked inside the 60s budget. (In an earlier state of the repo these same two checks got as far as asserting `done1["buildStatus"] == "SUCCEEDED"` and captured a terminal `buildStatus` 'FAULT' with `phases[] PROVISIONING/'FAULT'` and the emulator-authored reason 'this emulator does not run CodeBuild build containers, so the build could not be provisioned and no buildspec was executed'; that FAULT verdict is filed as 20261105T141707Z-codebuild-ecr-ecs-elbv2-logs-every-codebuild-build.md. The current run is the timeout variant and is owned by no filed issue, so it is filed here.)

Why: a client running the standard container CI/CD topology gets no working chain and, worse, no diagnosis: the calls neither succeed nor refuse, they simply never settle, so the client cannot distinguish an emulator limitation from a hung build, a stuck ECS rollout or an ALB that never becomes available. Every deploy-by-digest and circuit-breaker-rollback behaviour the two checks exist to exercise is unobservable.

Fix: the CodeBuild build-execution path behind StartBuild/BatchGetBuilds — the build phase state machine and whatever is meant to execute the buildspec — must advance a valid build to a terminal `buildStatus` within a bounded time and write the pushed image into the ECR repository, and the ECS/ELBv2 half of the chain (task placement, target registration, rollout state) must then settle so the checks reach their assertions instead of stalling. Related open issues for this same grouping carry other replay keys and do not own these timeouts: 20261105T141707Z-codebuild-ecr-ecs-elbv2-logs-every-codebuild-build.md (the PROVISIONING FAULT verdict), 20261105T082918Z-codebuild-ecr-ecs-elbv2-logs-the-circuit-breaker.md and 20261109T141609Z-codebuild-ecr-ecs-elbv2-logs-the-ecr-repository.md (the unserved dkr.ecr registry host announced to the chain).

Traces: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image, codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

## Evidence
Suite: service codebuild__ecr__ecs__elbv2__logs scored 0/2 (0%), 2 failed.

[mismatch] codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image
  Error: Check timed out after 60.0s

[mismatch] codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
  Error: Check timed out after 60.0s

Expected (AWS): StartBuild -> 200 with buildStatus 'IN_PROGRESS'; BatchGetBuilds polls to a terminal buildStatus 'SUCCEEDED' with all phases 'SUCCEEDED'; DescribeImages resolves the pushed tag to a 'sha256:' digest; the Fargate service pinned to repoUri@digest behind the ALB reaches rolloutState 'COMPLETED' and a GET to the ALB returns the body the buildspec baked in (token + build id); a second build deployed by digest makes the ALB serve build #2's body; a revision whose container never passes its health check reaches rolloutState 'FAILED', is rolled back, and the ALB keeps serving the previous bytes.
Actual (emulator): no step verdict at all — both checks exceed the 60s budget with no HTTP status, error code or message recorded, so none of the above is observed.
No status or error-code divergence is capturable here: the divergence is that the chain never settles. The project name, build id, token and generated resource names are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Create an ECR repository and two CloudWatch log groups (build logs, task logs) against the emulator.
2. Create an IAM role trusted by codebuild.amazonaws.com with ECR push/pull and logs permissions, and a task-execution role trusted by ecs-tasks.amazonaws.com with ECR pull permissions.
3. CreateProject with a buildspec that builds a tiny HTTP-serving image (baking a token, the build id and the tag into the served body) and docker-pushes it to that repository.
4. StartBuild with environmentVariablesOverride carrying the token, image tag and Dockerfile, then poll BatchGetBuilds for a terminal buildStatus.
5. Provision a public VPC, an ALB with a target group on tcp/80, register an awsvpc Fargate service pinned to repoUri@digest, and GET the ALB's DNS name.
   -> AWS: every step settles and the ALB returns the baked body; rolloutState reaches COMPLETED (and FAILED + rollback for the unhealthy revision).
   -> Emulator: the sequence never reaches an observable assertion within 60s.
6. Re-replay checks codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image and codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served to confirm.
