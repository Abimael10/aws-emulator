Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Commit: 78eeb93e75738c0cf0c5c47abcc6a48fc1935112

# ecs: a deployment-alarm rollout makes no observable progress — the alarm-driven rev2 failure and rollback to rev1 never happen (check cut at its 600 s budget)

> _Note: this issue was identified against the code base at commit `78eeb93e75738c0cf0c5c47abcc6a48fc1935112`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateService

## Description
Cause 1 of 1: a service created with a deploymentConfiguration.alarms block never produces the alarm-driven deployment outcome. After UpdateService moves the service to rev2 and the configured CloudWatch alarm transitions OK -> ALARM from real PutMetricData, the emulator never drives the rev2 deployment to rolloutState FAILED, never rolls the service back to rev1, and the positive-control twin never completes rev2; the whole scenario is cut off with 'Check timed out after 600.0s' without producing a single graded step. Failing check: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes.

AWS: Smithy: ECS DeploymentAlarms (members alarmNames, enable, rollback, all required) is documented as 'When the alarms are generated, Amazon ECS sets the service deployment to failed. Set the rollback parameter to have Amazon ECS to roll back your service to the last completed deployment after a failure', usable when the deploymentController is ECS. Deployment$rolloutState (enum COMPLETED | FAILED | IN_PROGRESS) carries the rollout state and Deployment$rolloutStateReason 'a description of the rollout state of a deployment' — for an alarm-tripped rollout the reason attributes the failure to the alarm, not to the circuit breaker. ServiceDeployment then reads status ROLLBACK_SUCCESSFUL (ServiceDeploymentStatus), carries a rollback record (Rollback: serviceRevisionArn + startedAt) whose serviceRevisionArn is one of the sourceServiceRevisions, and ServiceDeployment$alarms (ServiceDeploymentAlarms: status, alarmNames, triggeredAlarmNames — 'Amazon ECS considers the service deployment as failed when any of the alarms move to the ALARM state') reads status TRIGGERED with triggeredAlarmNames = the alarm that fired. A twin service whose alarm stays OK completes rev2 with a single COMPLETED deployment. The alarm/task counts, revision ARNs and names are all values from the request and state; the required behavior is the state machine above.

Emulator: the replay reports only 'Error: Check timed out after 600.0s' — no response diverged, nothing converged. None of the observable waypoints is reached inside the budget: the subject's rev2 deployment is never seen with rolloutState FAILED, the service never settles back to a single COMPLETED rev1 deployment, the twin is never seen COMPLETED on rev2, and the ServiceDeployment record for rev2 never reads ROLLBACK_SUCCESSFUL with alarms.status TRIGGERED.

Why: a client that protects a rollout with CloudWatch alarms gets no protection and no signal: a breaching alarm neither fails the deployment nor rolls the service back, and a reader polling DescribeServices / DescribeServiceDeployments waits forever instead of seeing FAILED + ROLLBACK_SUCCESSFUL. Deployment safety automation built on deployment alarms silently believes a bad revision succeeded, and the diagnostic fields (rolloutStateReason, triggeredAlarmNames, rollback) that tell an operator which alarm stopped the rollout are never populated.

Fix: the ECS service-deployment state machine behind CreateService/UpdateService — the alarm monitor that must poll the service's configured alarms during a rolling update, set the deployment's rolloutState to FAILED with an alarm-attributed rolloutStateReason, perform the rollback to the last completed service revision, and the ServiceDeployment/ServiceRevision projection that must record status ROLLBACK_SUCCESSFUL, the rollback record and alarms.status TRIGGERED with triggeredAlarmNames.

Traces: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes. Note: the sibling circuit-breaker rollback check (ecs.deployment-circuit-breaker-rolls-back-to-last-good) is already OWNED by 20261122T042537Z-ecs-5-net-new-causes-capacity-provider-base.md; the alarm-driven path in this issue was reported as a surviving failure and that issue is named for the fixer to deduplicate the shared rollback machinery against.

## Evidence
Expected (AWS): after the subject's alarm goes OK -> ALARM during the rev2 rollout, DescribeServices shows the rev2 deployment with rolloutState 'FAILED' and a rolloutStateReason naming the alarm (not the circuit breaker); the service then has one deployment, rolloutState 'COMPLETED', taskDefinition = rev1, runningCount 1; DescribeServiceDeployments for the rev2 deployment reads status 'ROLLBACK_SUCCESSFUL' with alarms {status: 'TRIGGERED', triggeredAlarmNames: [<the subject alarm>]} and a rollback record whose serviceRevisionArn is one of sourceServiceRevisions and runs rev1; the twin, whose alarm is never fed, stays on rev2 with a single COMPLETED deployment.

Actual (emulator): 'Error: Check timed out after 600.0s' — no FAILED rollout state, no rollback, no ROLLBACK_SUCCESSFUL ServiceDeployment and no TRIGGERED alarm monitor were observable at any poll, so no status, code or message could be compared.

## Steps to Reproduce
1. CreateCluster; RegisterTaskDefinition twice in one family (rev1 and rev2, FARGATE/awsvpc).
2. PutMetricAlarm two alarms over a custom namespace (one 'subject', one 'twin'), both reading OK.
3. CreateService twice on rev1 with deploymentConfiguration.alarms = {alarmNames: [<that service's alarm>], enable: true, rollback: true} and an awsvpc network configuration; wait until both show a single COMPLETED rev1 deployment with runningCount 1.
4. UpdateService both services to rev2.
5. PutMetricData repeatedly for the subject's metric only, so the subject alarm transitions OK -> ALARM while the twin's stays OK.
6. Poll DescribeServices and DescribeServiceDeployments.
Observed: nothing advances — no rev2 deployment ever reads rolloutState FAILED, no rollback to rev1 happens, the twin never reads COMPLETED on rev2, and the run is killed at the 600 s budget. Expected: the subject's rev2 deployment FAILED with an alarm-attributed rolloutStateReason, the service back on rev1 (COMPLETED), its rev2 ServiceDeployment ROLLBACK_SUCCESSFUL with alarms.status TRIGGERED and triggeredAlarmNames = [subject alarm], and the twin COMPLETED on rev2.
