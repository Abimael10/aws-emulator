Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop
Issue-Step: task#16
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop ecs-ecsdep-traj-redeploy-same-revision-vs-force-new-deployment ecs-ecsdep-traj-service-deployment-read-mistakes
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|ListServiceDeployments|2xx->2xx|$.serviceDeployments[*].targetServiceRevisionArn
Issue-Key: ecs|DescribeServiceDeployments|2xx->2xx|$.serviceDeployments[*].sourceServiceRevisions[*]
Issue-Key: ecs|DescribeServiceDeployments|2xx->2xx|$.serviceDeployments[*].targetServiceRevision.arn
Issue-Key: ecs|DescribeServiceDeployments|4xx->2xx|$.__type
Issue-Key: ecs|DescribeServiceDeployments|4xx->2xx|$.message
Issue-Key: ecs|DescribeServiceDeployments|4xx->2xx|status
Issue-Key: ecs|StopServiceDeployment|4xx->4xx|$.message
Issue-Key: ecs|StopServiceDeployment|4xx->4xx|$.__type
Issue-Key: ecs|DescribeServiceRevisions|4xx->2xx|$.__type
Issue-Key: ecs|DescribeServiceRevisions|4xx->2xx|$.message
Issue-Key: ecs|DescribeServiceRevisions|4xx->2xx|status
Issue-Related: 20261103T150504Z-ecs-service-responses-drop-deployment-configuration-task-sets.md
Issue-Related: 20261110T175605Z-ecs-service-deployments-never-reflect-the-circuit-breaker.md
Issue-Commit: 02c6380a70f664dba771786c55045481408bc4cc

# ECS service-deployment APIs never record a circuit-breaker rollback and miss or misclassify four deployment refusals

> _Note: this issue was identified against the code base at commit `02c6380a70f664dba771786c55045481408bc4cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServiceDeployments

## Description
The service-deployment surface (ListServiceDeployments / DescribeServiceDeployments / DescribeServiceRevisions / StopServiceDeployment) diverges in two ways: a deployment that the deployment circuit breaker failed is recorded as a plain success with no rollback, and four refusals on this surface are either not raised at all or raised with the wrong exception class and message. The open issues 20261110T175605Z-ecs-service-deployments-never-reflect-the-circuit-breaker.md and 20261122T042537Z-ecs-5-net-new-causes-capacity-provider-base.md describe the circuit breaker elsewhere, and 20261116T130854Z-ecs-misclassifies-four-service-and-deployment-refusals-clusternotfoundexception.md covers other refusals on this surface; none of them carries the replay key for the six causes below. 6 causes.

Cause 1 of 6: ListServiceDeployments reports a rolled-back deployment as SUCCESSFUL and lists a stale extra deployment.
AWS: Smithy: ListServiceDeployments output `serviceDeployments` -> ServiceDeploymentsBrief; the status member targets ServiceDeploymentStatus, whose value set is [PENDING, SUCCESSFUL, STOPPED, STOP_REQUESTED, IN_PROGRESS, ROLLBACK_REQUESTED, ROLLBACK_IN_PROGRESS, ROLLBACK_SUCCESSFUL, ROLLBACK_FAILED], and ServiceDeployment.statusReason is documented as "Information about why the service deployment is in the current status. For example, the circuit breaker detected a failure." A deployment that the circuit breaker failed and rolled back therefore ends in status ROLLBACK_SUCCESSFUL with the rollback reason as statusReason, and its targetServiceRevisionArn is the revision that was being deployed; the prior deployment keeps status SUCCESSFUL. The recorded response shows exactly two deployments, the newest with status 'ROLLBACK_SUCCESSFUL' and statusReason 'Service deployment rolled back because the circuit breaker threshold was exceeded.'.
Emulator: the newest deployment carries status 'SUCCESSFUL' with statusReason 'ECS deployment <ecs-svc deployment id> completed.', the two entries' target service-revision ARNs are swapped relative to the recording, the second entry is reported 'ROLLBACK_SUCCESSFUL', and a third `serviceDeployments[2]` entry is returned that AWS does not list (status 200 both sides). Fixed text of the required statusReason is "Service deployment rolled back because the circuit breaker threshold was exceeded."; it has no variable part.
Why: a client polling ListServiceDeployments to gate a release sees the failed deployment as a success, so it promotes a revision that AWS rolled back, and its deployment history contains a deployment AWS never reports.
Fix: the service-deployment state machine (the circuit-breaker evaluation that terminates a deployment) must set ROLLBACK_SUCCESSFUL plus the rollback statusReason and must not append a third record for a single rollback.
Traces: ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#16.

Cause 2 of 6: DescribeServiceDeployments omits the `rollback` structure and swaps source/target revisions for the same deployment.
AWS: Smithy: ServiceDeployment.rollback -> Rollback, documented as "The rollback options the service deployment uses when the deployment fails", with members reason, startedAt and serviceRevisionArn; ServiceDeployment.sourceServiceRevisions is "The currently deployed workload configuration" and targetServiceRevision is "The workload configuration being deployed"; ServiceDeployment.deploymentCircuitBreaker -> ServiceDeploymentCircuitBreaker is "The circuit breaker configuration that determines a service deployment failed". For a circuit-breaker rollback, AWS returns status ROLLBACK_SUCCESSFUL, statusReason 'Service deployment rolled back because the circuit breaker threshold was exceeded.', `rollback` populated with that reason and the revision ARN rolled back to, sourceServiceRevisions naming the previously deployed revision and targetServiceRevision naming the revision that failed.
Emulator: `rollback` is absent, status is 'SUCCESSFUL' with statusReason 'ECS deployment <id> completed.', sourceServiceRevisions[0] names the failed (new) revision while targetServiceRevision.arn names the old one — i.e. the two are inverted — and deploymentCircuitBreaker.status diverges (status 200 both sides).
Why: a client diagnosing a failed rollout cannot see that a rollback happened or which revision it landed on, so automated remediation treats a rolled-back service as healthy at the new revision.
Fix: the DescribeServiceDeployments handler's rendering of the stored deployment, and the rollback bookkeeping in the deployment state machine that should populate Rollback and orient source vs target revisions.
Traces: ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#17.

Cause 3 of 6: DescribeServiceDeployments accepts ARNs spanning two services instead of refusing the request.
AWS: Smithy: DescribeServiceDeployments input `serviceDeploymentArns` (required, StringList) and modeled errors include InvalidParameterException (HTTP 400). AWS requires every requested deployment ARN to belong to one service and refuses a mixed batch with 400 InvalidParameterException and message 'Cross service requests are not allowed. All requested service deployments must be for a single service' — fixed text, no variable part.
Emulator: answers 200 with a deployment list and no `__type`/`message` at all.
Why: a client that batches deployment ARNs across services gets results that AWS would never return, so a tool built on the refusal (splitting a batch per service) is never exercised and cross-service reads silently succeed.
Fix: DescribeServiceDeployments input validation, before resolution of the ARNs.
Traces: ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#20.

Cause 4 of 6: StopServiceDeployment's invalid-stopType refusal uses the emulator's own wording.
AWS: Smithy: StopServiceDeployment input `stopType` -> StopServiceDeploymentStopType, value set [ABORT, ROLLBACK]; modeled errors include InvalidParameterException (400). AWS refuses an out-of-set stopType with 400 and message 'The StopType is invalid.' — fixed text, no variable part.
Emulator: returns 400 with message 'stop type should be one of [ABORT,ROLLBACK]'.
Why: a client matching on AWS's message text to distinguish an invalid stopType from other InvalidParameterException causes never matches, so the error is handled as an unknown failure.
Fix: the StopServiceDeployment handler's stopType validation message.
Traces: ecs-ecsdep-traj-redeploy-same-revision-vs-force-new-deployment#task#14.

Cause 5 of 6: StopServiceDeployment with stopType ABORT answers ConflictException instead of UnsupportedFeatureException.
AWS: Smithy: StopServiceDeployment models both UnsupportedFeatureException and ConflictException at HTTP 400. AWS does not support the ABORT stopType and refuses it with UnsupportedFeatureException and message 'ECS StopServiceDeployment API with ABORT stopType is not supported.' — fixed text; the only variable part of the alternative Conflict message is the deployment ARN, which is not what AWS returns here.
Emulator: returns 400 ConflictException with message 'ServiceDeployment is stopping or has already finished for arn: <serviceDeploymentArn>', i.e. it reports a state conflict rather than an unsupported feature.
Why: a client branching on the exception type retries or waits (the correct reaction to ConflictException) instead of concluding ABORT is unavailable and falling back to ROLLBACK, so the call loops.
Fix: the StopServiceDeployment handler must reject stopType ABORT with UnsupportedFeatureException before it evaluates the deployment's current state.
Traces: ecs-ecsdep-traj-redeploy-same-revision-vs-force-new-deployment#task#15.

Cause 6 of 6: DescribeServiceRevisions accepts a malformed service ARN instead of refusing it.
AWS: Smithy: DescribeServiceRevisions input `serviceRevisionArns` (required, StringList) with modeled InvalidParameterException (400). AWS validates the ARN shape and refuses with 400 InvalidParameterException and message 'The Service ARN provided is invalid.' — fixed text, no variable part.
Emulator: answers 200 with no `__type`/`message`.
Why: a client passing a mistyped or wrongly-typed ARN gets a success and an empty/absent revision rather than a validation error, so the bug surfaces far from its cause.
Fix: DescribeServiceRevisions input validation (ARN parsing for each requested revision ARN).
Traces: ecs-ecsdep-traj-service-deployment-read-mistakes#task#17.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout.

Cause 1 — ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#16, ListServiceDeployments, expected 200 / actual 200: "$.serviceDeployments[0].status: value_mismatch (expected 'ROLLBACK_SUCCESSFUL', got 'SUCCESSFUL')"; "$.serviceDeployments[0].statusReason: value_mismatch (expected 'Service deployment rolled back because the circuit breaker threshold was exceeded.', got 'ECS deployment ecs-svc/... completed.')"; targetServiceRevisionArn values swapped between entries [0] and [1]; "$.serviceDeployments[2]: extra (expected None, got {...status 'SUCCESSFUL'...})".

Cause 2 — ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#17, DescribeServiceDeployments, expected 200 / actual 200: "$.serviceDeployments[0].rollback: missing (expected {'reason': 'Service deployment rolled back because the circuit breaker threshold was exceeded.', 'serviceRevisionArn': ..., 'startedAt': ...}, got None)"; status SUCCESSFUL vs ROLLBACK_SUCCESSFUL; sourceServiceRevisions[0].arn and targetServiceRevision.arn inverted; deploymentCircuitBreaker.status diverges.

Cause 3 — ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#20, DescribeServiceDeployments, expected 400 / actual 200: "$.__type: missing (expected 'InvalidParameterException', got None)"; "$.message: missing (expected 'Cross service requests are not allowed. All requested service deployments must be for a single service', got None)".

Cause 4 — ecs-ecsdep-traj-redeploy-same-revision-vs-force-new-deployment#task#14, StopServiceDeployment, expected 400 / actual 400: "$.message: value_mismatch (expected 'The StopType is invalid.', got 'stop type should be one of [ABORT,ROLLBACK]')".

Cause 5 — ecs-ecsdep-traj-redeploy-same-revision-vs-force-new-deployment#task#15, StopServiceDeployment, expected 400 / actual 400: "$.__type: value_mismatch (expected 'UnsupportedFeatureException', got 'ConflictException')"; "$.message: value_mismatch (expected 'ECS StopServiceDeployment API with ABORT stopType is not supported.', got 'ServiceDeployment is stopping or has already finished for arn: arn:aws:ecs:...:service-deployment/...')".

Cause 6 — ecs-ecsdep-traj-service-deployment-read-mistakes#task#17, DescribeServiceRevisions, expected 400 / actual 200: "$.__type: missing (expected 'InvalidParameterException', got None)"; "$.message: missing (expected 'The Service ARN provided is invalid.', got None)".

Model evidence: smithy_query shape=ServiceDeploymentStatus -> [PENDING, SUCCESSFUL, STOPPED, STOP_REQUESTED, IN_PROGRESS, ROLLBACK_REQUESTED, ROLLBACK_IN_PROGRESS, ROLLBACK_SUCCESSFUL, ROLLBACK_FAILED]; shape=ServiceDeployment -> members rollback (-> Rollback), deploymentCircuitBreaker, sourceServiceRevisions, targetServiceRevision, statusReason; shape=Rollback -> reason/startedAt/serviceRevisionArn; shape=StopServiceDeploymentStopType -> [ABORT, ROLLBACK]; operation=StopServiceDeployment errors include UnsupportedFeatureException and ConflictException; operation=DescribeServiceDeployments/DescribeServiceRevisions errors include InvalidParameterException.

## Steps to Reproduce
1. Replay fixture ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop: create a service with deployment circuit breaker and rollback enabled, deploy a revision that fails, then at step task#16 call ListServiceDeployments and at task#17 DescribeServiceDeployments — status/statusReason/rollback and the source/target revisions diverge, and an extra deployment is listed.
2. Same fixture, step task#20: call DescribeServiceDeployments with deployment ARNs belonging to two different services — a 200 is returned instead of 400 InvalidParameterException.
3. Replay fixture ecs-ecsdep-traj-redeploy-same-revision-vs-force-new-deployment, step task#14: StopServiceDeployment with an out-of-set stopType — the message is the emulator's own. Step task#15: StopServiceDeployment with stopType ABORT — ConflictException instead of UnsupportedFeatureException.
4. Replay fixture ecs-ecsdep-traj-service-deployment-read-mistakes, step task#17: DescribeServiceRevisions with a malformed service-revision ARN — a 200 is returned instead of 400 InvalidParameterException.
