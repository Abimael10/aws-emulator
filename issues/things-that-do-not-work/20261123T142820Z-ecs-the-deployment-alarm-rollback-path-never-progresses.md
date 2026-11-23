Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Commit: 94f7b27553b727586aac9de0add3c17de28dbaa9

# ecs: the deployment-alarm rollback path never progresses — an OK->ALARM deployment alarm does not drive the rev2 rollout to rolloutState FAILED, so no rollback to rev1 and no ROLLBACK_SUCCESSFUL ServiceDeployment is ever observable

> _Note: this issue was identified against the code base at commit `94f7b27553b727586aac9de0add3c17de28dbaa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateService

## Description
Cause 1 of 1: ECS accepts and echoes a service's deploymentConfiguration.alarms, but nothing downstream of that configuration ever happens. Two services created from rev1 with a deployment alarm each are updated to rev2; the subject's alarm is driven OK->ALARM with real PutMetricData while the twin's alarm is never fed. The scenario then makes no further observable progress and the check exhausts the runner's 600 s ceiling without reaching a verdict. Failing check: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes.

AWS: Smithy — CreateService/UpdateService carry DeploymentConfiguration.alarms (DeploymentAlarms: alarmNames, enable, rollback); DescribeServices returns Deployment.rolloutState (DeploymentRolloutState enum: COMPLETED, FAILED, IN_PROGRESS) and Deployment.rolloutStateReason; DescribeServiceDeployments/DescribeServiceDeployment return ServiceDeployment.status (including ROLLBACK_SUCCESSFUL), ServiceDeployment.alarms (ServiceDeploymentAlarms: status, triggeredAlarmNames) and ServiceDeployment.rollback (Rollback: serviceRevisionArn, startedAt, reason). The model defines the members but not the monitor's timing, so the mechanism is recorded behavior only and matches AWS's documented deployment-alarm behavior (deployment-alarm-failure): while a deployment is in progress ECS watches the named alarms, and an OK->ALARM transition of one of them fails that deployment — the new revision's deployment reports rolloutState FAILED with a rolloutStateReason that attributes the failure to the ALARM (not to the circuit breaker) — and, with rollback=true, ECS then rolls the service back so the surviving single deployment is the previous revision in rolloutState COMPLETED, and the failed revision's ServiceDeployment reads status ROLLBACK_SUCCESSFUL with alarms.status TRIGGERED, alarms.triggeredAlarmNames == the alarm that fired, and a rollback record naming the restored serviceRevisionArn and a startedAt. A service whose alarm never leaves OK completes its new revision normally with no rollback — that is the positive control in the same cluster.

Emulator: the configuration round-trip is correct — DescribeServices returns deploymentConfiguration.alarms with alarmNames == [the created alarm], enable == true and rollback == true for both services, and those assertions pass. Past that point the scenario never advances to any of the states above within the time the runner allows, so the emulator never shows: a rev2 deployment reaching rolloutState FAILED, an alarm-attributed rolloutStateReason, a rollback to rev1, or a ServiceDeployment with status ROLLBACK_SUCCESSFUL / alarms.status TRIGGERED / a rollback record. The failure is a stall, not a wrong value: the check's every wait is a bounded poll and the whole check is cut off at the harness's 600 s limit. Hypothesis: ECS's deployment-alarm monitor is not implemented at all — the alarms block is stored and echoed but no component subscribes to those alarms during a deployment — and/or the services' rev1 deployments never reach the single COMPLETED deployment with runningCount 1 that the scenario starts from.

Why: deployment alarms are the supported way to gate an ECS rollout on a real application signal (5xx rate, latency, error count) rather than on container health alone. A client relying on them gets the opposite of the guarantee it bought: a rollout that AWS would fail and roll back proceeds unchallenged, the bad revision stays live, and the service's deployment history shows no failure and no rollback to explain it. Any deploy pipeline that waits for rolloutState FAILED or for ServiceDeployment ROLLBACK_SUCCESSFUL, or that distinguishes an alarm-tripped rollback from a circuit-breaker one by rolloutStateReason, blocks forever and then reports a green deploy.

Fix: the ECS service deployment lifecycle — specifically the deployment-alarm monitor and the rollback path it drives (the component that, while a deployment is IN_PROGRESS, watches deploymentConfiguration.alarms.alarmNames for an OK->ALARM transition, sets the deployment's rolloutState FAILED with an alarm-attributed rolloutStateReason, performs the rollback when rollback=true, and records it on the ServiceDeployment as status ROLLBACK_SUCCESSFUL with alarms.status TRIGGERED, triggeredAlarmNames and a rollback{serviceRevisionArn,startedAt}). The storage/echo of the alarms configuration already works and needs no change.

Traces: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes — 'Check timed out after 600.0s', after the deploymentConfiguration.alarms round-trip assertions passed.

Related but distinct open issues, none of which carries this check's replay key: 20261122T202428Z-ecs-service-deployment-apis-never-record-a-circuit.md and 20261110T175605Z-ecs-service-deployments-never-reflect-the-circuit-breaker.md (both about the CIRCUIT BREAKER, a different failure trigger that this check explicitly requires the rolloutStateReason NOT to name), and 20261113T124512Z-ecs-logs-a-service-deployment-whose-container-never.md (a container that never passes its health check).

## Evidence
Expected (AWS), in order, for two services in one cluster each created from rev1 with deploymentConfiguration.alarms={alarmNames:[<its own alarm>], enable:true, rollback:true}:
1. Both reach a single deployment of rev1, rolloutState COMPLETED, runningCount 1.
2. Both alarms read StateValue 'OK'.
3. UpdateService both to rev2; PutMetricData breaching values on the SUBJECT's metric only.
4. The subject's rev2 deployment reaches rolloutState 'FAILED' and its rolloutStateReason mentions the alarm and does NOT mention the circuit breaker.
5. The subject settles back to a single COMPLETED deployment of rev1 (service taskDefinition == rev1) while the twin settles on a single COMPLETED deployment of rev2.
6. The subject alarm's history contains a '... to ALARM' transition; the twin alarm's history contains none.
7. The subject's rev2 ServiceDeployment reads status 'ROLLBACK_SUCCESSFUL', alarms.status 'TRIGGERED', alarms.triggeredAlarmNames == [the subject alarm], and a rollback record with a serviceRevisionArn (present in sourceServiceRevisions) and a startedAt.

Actual (emulator): step 0 passes — DescribeServices returns deploymentConfiguration.alarms with alarmNames == [the service's alarm], enable == true, rollback == true for both services. Nothing from steps 1-7 is ever observed: the check is cut off by the harness.
Error: "Check timed out after 600.0s". Every wait in the check is a bounded poll (steady state 900 s, alarms-OK 600 s, rev2-FAILED 1500 s, settle 1500 s), so the timeout means the emulator never produced the next expected state, not that an assertion compared unequal values.

Model citations used: DeploymentConfiguration.alarms -> DeploymentAlarms{alarmNames, enable, rollback}; Deployment.rolloutState -> DeploymentRolloutState{COMPLETED, FAILED, IN_PROGRESS}; Deployment.rolloutStateReason; ServiceDeployment.status (ROLLBACK_SUCCESSFUL), ServiceDeployment.alarms -> ServiceDeploymentAlarms{status, triggeredAlarmNames}, ServiceDeployment.rollback -> Rollback{serviceRevisionArn, startedAt, reason}.

## Steps to Reproduce
1. Ensure the ECS service-linked role exists; create a VPC with public subnets and a security group.
2. CreateCluster. RegisterTaskDefinition twice in one family to get two distinct revisions rev1 and rev2 (e.g. commands 'sleep 3600' and 'sleep 3601').
3. PutMetricAlarm twice over a custom namespace, one alarm per leg, dimensioned so only the 'subject' leg's metric can be written (treat-missing-data notBreaching, Period 60).
4. CreateService twice from rev1 (Fargate, awsvpcConfiguration with assignPublicIp ENABLED, desiredCount 1), each with deploymentConfiguration.alarms={alarmNames:[its own alarm], enable:true, rollback:true}.
5. Poll DescribeServices until both show one deployment of rev1, rolloutState COMPLETED, runningCount 1; poll DescribeAlarms until both alarms read 'OK'.
6. UpdateService both services to rev2.
7. PutMetricData a breaching value on the SUBJECT's dimension every 20 s (do NOT use SetAlarmState — ECS reacts to a real OK->ALARM transition).
8. Poll DescribeServices for the subject's rev2 deployment to report rolloutState 'FAILED' with an alarm-attributed rolloutStateReason, then for the subject to settle back on rev1 COMPLETED while the twin settles on rev2 COMPLETED, then DescribeServiceDeployments/DescribeServiceDeployment for the subject's rev2 deployment to read ROLLBACK_SUCCESSFUL with alarms.status TRIGGERED. AWS walks all of these; the emulator produces none of them and the scenario never terminates.
