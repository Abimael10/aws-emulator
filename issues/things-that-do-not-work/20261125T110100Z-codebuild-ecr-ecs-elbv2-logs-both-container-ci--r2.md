Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Key: codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Commit: fcaef5045051e2f19f2ef72b682687f38c3f2a70

# codebuild__ecr__ecs__elbv2__logs: both container CI/CD-chain checks reach no observable step — the CodeBuild→ECR→ECS/ALB chain never produces a terminal buildStatus, a pullable digest, a healthy target, a served body or a deployment rolloutState

> _Note: this issue was identified against the code base at commit `fcaef5045051e2f19f2ef72b682687f38c3f2a70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartBuild

## Description
1 net-new cause, grounded in BOTH surviving failing checks of this service group (0/2, 0% this run). Neither check is marked ALREADY FILED or OWNED: the two open issues that mention them carry different replay keys (see 'Relation to open issues' below), so this run's failure is not covered by either.

Cause 1 of 1: the cross-service container CI/CD chain never advances to any assertable observation, so none of the documented steady states of the flow — CodeBuild build completion, the pushed image's ECR digest, ECS task placement and deployment rolloutState, ELBv2 target health, and the bytes served through the listener — is ever produced. Both checks are cut off by the harness budget with nothing captured ('Check timed out after 60.0s'): no HTTP status, no error code and no mismatched field exist, because the emulator never settles any of the states the flow polls for.
Failing check ids for this cause: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image, codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

AWS: Smithy: com.amazonaws.codebuild#StatusType = {SUCCEEDED, FAILED, FAULT, TIMED_OUT, IN_PROGRESS, STOPPED}; com.amazonaws.codebuild#BuildPhaseType = {SUBMITTED, QUEUED, PROVISIONING, DOWNLOAD_SOURCE, INSTALL, PRE_BUILD, BUILD, POST_BUILD, UPLOAD_ARTIFACTS, FINALIZING, COMPLETED}; com.amazonaws.ecs#DeploymentRolloutState = {COMPLETED, FAILED, IN_PROGRESS}. On AWS, StartBuild of a project whose buildspec succeeds returns buildStatus IN_PROGRESS and BatchGetBuilds polls it through those phases to a terminal SUCCEEDED; the image the buildspec docker-pushes is then addressable in ECR (DescribeImages returns an imageDigest beginning 'sha256:' with a non-zero imageSizeInBytes); an ECS Fargate service on a task definition pinning <repositoryUri>@<digest> reaches one RUNNING task whose address the target group reports 'healthy'; a GET through the ALB listener returns HTTP 200 with the bytes the image bakes. For the rollback scenario, a revision whose image fails the target group's health check on a service with deploymentCircuitBreaker enabled reaches rolloutState FAILED with a circuit-breaker rolloutStateReason, the service rolls back to the last COMPLETED revision, and the load balancer keeps serving the previous revision's bytes throughout (AWS docs: ECS deployment circuit breaker). The requirement is the enum vocabulary and these transitions; project names, build ids, tags, digests, ports and addresses in any one run are values minted by that run and are not part of it. Recorded behavior only for the intermediate values of the scenario.

Emulator: nothing in the chain is answered observably. Both checks end with the harness cutting them off at the run budget with no completed step and no captured response — no buildStatus, no image digest, no RUNNING task, no target-health entry, no ALB response and no rolloutState. Hypothesis (neither the model nor any captured response in this run states it): the first blocker is the CodeBuild build-execution gap this corpus records elsewhere, where a build walks [(SUBMITTED, SUCCEEDED), (QUEUED, SUCCEEDED), (PROVISIONING, FAULT), (COMPLETED, None)] and ends buildStatus FAULT instead of SUCCEEDED (so no image digest is ever produced and nothing downstream can exist); a second candidate is a provisioning/teardown poll in the networking setup (ALB, ECS service or route-table teardown) never settling and consuming the whole budget before StartBuild's outcome is read.

Why: building an image in CodeBuild, pushing it to ECR and rolling it out to an ECS service behind an ALB is the standard container CI/CD topology. A client gets neither the happy path (the ALB serving the body the newest build baked, with the deployment reporting rolloutState COMPLETED) nor the safety property (a bad build being rejected by the circuit breaker while the old bytes keep being served). Worse than a wrong answer, code that polls these documented steady states never terminates: it hangs instead of succeeding or failing.

Fix: the CodeBuild build-execution state machine behind StartBuild/BatchGetBuilds (phase progression and terminal buildStatus for a succeeding buildspec, including the privileged docker build/push it performs) must produce a terminal SUCCEEDED so the chain's first observation exists; then each downstream steady state this flow waits on must settle — the ECR image/digest written by the push, the ECS service's task placement plus its deployment rolloutState (including the circuit-breaker FAILED/rollback path and its rolloutStateReason), the ELBv2 target-group health evaluation, and the listener data path that serves the task's bytes.

Relation to open issues (neither covers these checks this run): 20261105T082918Z-codebuild-ecr-ecs-elbv2-logs-the-circuit-breaker.md describes the same timeout for the rollback check but carries the replay key '_delete_net#2|Timeout|_time.sleep(2)', not this run's; 20261124T134157Z-codebuild-ecr-ecs-elbv2-logs-ecr-repositoryuri-announces-registryid-dkr-ecr-region-localhost.md lists both checks but keys them on the earlier published-URL assertion ('run#2|AssertionError|ctx.published_url_mismatch'), which is no longer where they fail. The CodeBuild PROVISIONING-FAULT family (20261022T172625Z-codebuild-codepipeline-ecr-ci-chain-pipeline-executions-never.md, 20261115T101606Z-codebuild-codepipeline-ecr-3-net-new-causes-privileged.md) carries none of this service group's keys.

Traces: codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image, codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served.

## Evidence
Suite run (342/434 passed, 78.8%): service codebuild__ecr__ecs__elbv2__logs scored 0/2 (0%), 2 failed. Neither failure is marked ALREADY FILED or OWNED.

[mismatch] codebuild__ecr__ecs__elbv2__logs.cicdchain-alb-body-tracks-the-latest-codebuild-image
  Error: Check timed out after 60.0s
[mismatch] codebuild__ecr__ecs__elbv2__logs.cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served
  Error: Check timed out after 60.0s

Expected (AWS): StartBuild returns buildStatus 'IN_PROGRESS'; BatchGetBuilds polls the build to the terminal 'SUCCEEDED' with each phase phaseStatus 'SUCCEEDED'; DescribeImages for the pushed tag returns an imageDigest beginning 'sha256:' with non-zero size; an ECS Fargate service on a task definition pinning <repositoryUri>@<digest> reaches exactly one RUNNING task whose address the target group reports 'healthy'; a GET through the ALB listener returns HTTP 200 whose body is the token + build id + tag the image baked, and that deployment reports rolloutState 'COMPLETED'. A second build deployed by digest makes the ALB serve build #2's body. In the rollback scenario, a revision whose image fails the health check reaches rolloutState 'FAILED' with a circuit-breaker rolloutStateReason, the service rolls back to the last COMPLETED revision, and the ALB keeps serving the previous build's bytes throughout.

Actual (emulator): none of these assertions is reached in either check. No build status, image digest, task, target-health entry, ALB response or rolloutState was captured — the harness cut both checks off at the run budget with no completed step, so there is no status code, error code or field value to compare. The invariant that fails is the existence and the transitions of the documented states (IN_PROGRESS -> SUCCEEDED; rolloutState IN_PROGRESS -> COMPLETED / FAILED; target health 'healthy'), not any one run's project name, build id, tag, digest, host or port.

## Steps to Reproduce
1. Point an AWS client at the emulator. CreateRepository in ECR and create two CloudWatch log groups (one for the build, one for the ECS tasks).
2. Create a CodeBuild service role trusted by codebuild.amazonaws.com with ECR push/pull and logs permissions, and an execution role trusted by ecs-tasks.amazonaws.com with ECR pull permissions.
3. CreateProject with privilegedMode and a buildspec that docker-builds a tiny HTTP-serving image (baking a token, the build id and the tag into the body it serves) and pushes it to that repository under tag v1.
4. StartBuild with environmentVariablesOverride carrying the token, the tag and the Dockerfile; assert the returned buildStatus is 'IN_PROGRESS'.
5. Create a public VPC with subnets, an ALB security group open on tcp/80, a task security group admitting tcp/80 only from the ALB group, a target group, an ALB and an HTTP listener forwarding to it; create an ECS cluster.
6. Poll BatchGetBuilds until the build leaves IN_PROGRESS and assert buildStatus == 'SUCCEEDED', then DescribeImages for tag v1 and assert a 'sha256:'-prefixed digest with non-zero size.
   -> AWS: the build SUCCEEDS and the image is addressable by digest. Emulator: no terminal build outcome or digest is ever observed; the flow never settles and the check is cut off at the harness budget with nothing captured.
7. RegisterTaskDefinition pinning <repositoryUri>@<digest>, CreateService on it behind the target group (deploymentCircuitBreaker enabled), and poll for one RUNNING task, a 'healthy' target, rolloutState 'COMPLETED', and an HTTP 200 through the ALB whose body equals the baked body.
8. Build and deploy a second image by digest and poll for the ALB to serve build #2's body; then deploy a revision whose image fails the target group's health check and poll for that deployment's rolloutState 'FAILED' with the circuit-breaker rolloutStateReason, for the rollback to the last COMPLETED revision, and for the ALB to have kept serving the previous build's bytes.
9. Re-replay checks cicdchain-alb-body-tracks-the-latest-codebuild-image and cicdchain-unhealthy-build-rollout-rejected-and-old-bytes-still-served to confirm the fix.
