Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__cloudwatch.alarm-monitor-role-not-assumable-deployment-rejected appconfig__cloudwatch.cloudwatch-alarm-rollback-restores-previous-version
Issue-Key: appconfig__cloudwatch.alarm-monitor-role-not-assumable-deployment-rejected|_wait_terminal#31|AssertionError|raise AssertionError(
Issue-Key: appconfig__cloudwatch.cloudwatch-alarm-rollback-restores-previous-version|_wait_terminal#37|Timeout|time.sleep(2.0)
Issue-Commit: d38a11298534a5b4ed5fe1ad46400f3e9411729b

# appconfig__cloudwatch: GetDeployment serves Deployment$State as 'Complete' instead of the modelled 'COMPLETE', so a monitored deployment's settle barrier never releases and neither the monitor-role refusal nor the alarm-driven rollback is reached

> _Note: this issue was identified against the code base at commit `d38a11298534a5b4ed5fe1ad46400f3e9411729b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployment

## Description
One net-new root cause in this service group.

Cause 1 of 1: Deployment$State is serialized in PascalCase ('Complete') rather than the enum value the model and the recording use ('COMPLETE'), so no client polling GetDeployment for a terminal state ever sees one, even though the deployment has in fact settled.
Failing check ids: appconfig__cloudwatch.alarm-monitor-role-not-assumable-deployment-rejected, appconfig__cloudwatch.cloudwatch-alarm-rollback-restores-previous-version.

AWS: Smithy (services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json) models Deployment$State -> DeploymentState, an enum whose values are exactly [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED], with no enumValue override — so the token on the wire is the SCREAMING_SNAKE value, and the terminal set is COMPLETE / ROLLED_BACK / REVERTED. The recording agrees: both checks poll GetDeployment for State in ('COMPLETE', 'ROLLED_BACK', 'REVERTED'). The sibling enum is NOT the same shape on the wire: Environment$State is modelled [READY_FOR_DEPLOYMENT, DEPLOYING, ROLLING_BACK, ROLLED_BACK, REVERTED] but the recording reads it as PascalCase — these checks assert GetEnvironment State == 'ReadyForDeployment' and treat ('Deploying', 'RollingBack') as the transitional environment states (recorded behavior only for that enum). Both of those environment assertions PASS against this emulator today, so the requirement is per-enum: Deployment$State must carry the SCREAMING_SNAKE value, Environment$State the PascalCase one. The requirement is the member -> wire-token mapping, not any one run's state.

Emulator: GetDeployment answers 200 with State 'Complete'. In appconfig__cloudwatch.alarm-monitor-role-not-assumable-deployment-rejected the correctly-trusted monitored deployment reaches its settled state and the poll still never matches: "AssertionError: deployment 1 never reached a terminal state within 480s; last 'Complete'" (_wait_terminal, which returns as soon as GetDeployment()['State'] is in ('COMPLETE','ROLLED_BACK','REVERTED')). appconfig__cloudwatch.cloudwatch-alarm-rollback-restores-previous-version uses the same _TERMINAL tuple and the same _wait_terminal barrier and is killed by the harness at its cap instead: "Error: Check timed out after 600.0s". No error code or status diverges — the call is a 200 — the diverging value is the State token itself.

Why: State is the only completion signal AppConfig gives a deployment. Every safe-deployment flow built on it — a deploy script or CI gate waiting for COMPLETE before promoting, a Step Functions/EventBridge poller, an auto-rollback watcher waiting for ROLLED_BACK, a teardown that must not delete an in-flight environment — compares State against the value AWS sends, so against this emulator the comparison is false for every state and the client blocks until its own timeout. Here it also makes two distinct AppConfig behaviours unobservable: a deployment whose Monitors AlarmRoleArn cannot be assumed by appconfig.amazonaws.com must terminate ROLLED_BACK (or be refused up front) rather than complete, and a deployment whose monitored CloudWatch alarm goes into ALARM during the bake window must roll back and leave the environment serving the previous configuration version. Neither leg is reached, so neither can be confirmed or refuted from this replay.

Fix: the appconfig response serializer / enum-value mapping for DeploymentState, applied at the wire boundary in one shared place rather than per handler, so every State-bearing response (GetDeployment, StartDeployment, StopDeployment, ListDeployments items, and DeploymentSummary) carries the SCREAMING_SNAKE value, and the same token appears wherever a deployment State is interpolated into a message. It must NOT be flipped for Environment$State, whose PascalCase form these checks already accept. Open issues that describe the same seam but do NOT carry either of these checks' replay keys and do not cover this service group: 20261115T214126Z-appconfig-appconfigdata-getdeployment-serves-deployment-state-in-pascalcase.md and 20261115T214126Z-appconfigdata-appconfig-serves-deployment-state-in-pascalcase-complete-instead.md (same direction, filed for appconfig / appconfig__appconfigdata), 20261115T213820Z-appconfig-lambda-2-net-new-causes-getdeployment-serves.md (same direction, appconfig__lambda), and 20261110T191755Z-appconfig-getdeployment-getenvironment-serve-state-as-the-model.md, which asserts the OPPOSITE direction for Deployment$State (that 'Complete' is the AWS token) and should be reconciled against the model's enum values and these checks' terminal set before a fix is written, or the fix will keep oscillating. Also related but not covering these keys: 20261025T020208Z-appconfig-cloudwatch-appconfig-deployments-with-cloudwatch-alarm-monitors-never-reach.md, filed from a different corpus against a state machine that never settled at all; it is now out of date, because the deployment DOES settle here and only its serialized token is wrong.

Traces: appconfig__cloudwatch.alarm-monitor-role-not-assumable-deployment-rejected (quotes the served token: last 'Complete'), appconfig__cloudwatch.cloudwatch-alarm-rollback-restores-previous-version (same barrier, killed at 600 s).

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the booted emulator: 907/1226 passed (74.0%); 268 mismatched, 51 errored, 0 skipped; bootstrap_signal false. Per-service appconfig__cloudwatch 0/2 (0%), 2 failed; neither is marked ALREADY FILED nor OWNED by an open issue in this replay.

[mismatch] appconfig__cloudwatch.alarm-monitor-role-not-assumable-deployment-rejected
  File "check_appconfig__cloudwatch_alarm_monitor_role_not_assumable_deployment_rejected.py", line 287, in run
    settled = _wait_terminal(ac, app_id, good_env, dep, timeout_s=_TERMINAL_TIMEOUT_S)
  File "...", line 149, in _wait_terminal
  AssertionError: deployment 1 never reached a terminal state within 480s; last 'Complete'

[mismatch] appconfig__cloudwatch.cloudwatch-alarm-rollback-restores-previous-version
  Error: Check timed out after 600.0s

From the check sources in the replay report, both define _TERMINAL = ("COMPLETE", "ROLLED_BACK", "REVERTED") and _ENV_TRANSITIONAL = ("Deploying", "RollingBack"), and both poll:
  _wait_terminal: while deadline: last = ac.get_deployment(ApplicationId, EnvironmentId, DeploymentNumber); if last.get("State") in _TERMINAL: return last; sleep(4.0)
The first check's earlier, PASSING assertions pin the environment enum in the opposite casing and show the emulator already matches there:
  assert broken["Monitors"] == [{AlarmArn, AlarmRoleArn}]  (CreateEnvironment stores an unassumable AlarmRoleArn verbatim) — passed
  assert broken["State"] == "ReadyForDeployment"  — passed

Expected (AWS): GetDeployment on the settled, correctly-monitored deployment returns State 'COMPLETE' (one of the modelled DeploymentState values), the barrier releases in seconds, and the check proceeds to its graded legs — the data plane serving the deployed bytes, then the broken-monitor-role deployment terminating ROLLED_BACK rather than COMPLETE.
Actual (emulator): State 'Complete' — PascalCase, which is not a DeploymentState value — returned for 480 s of polling in check 1, and for the whole 600 s budget in check 2. Everything the two checks exist to grade (the monitor-role refusal/rollback, and the alarm-triggered rollback restoring the previous hosted configuration version) is left ungraded.

Smithy consulted (services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json): DeploymentState = enum values [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED]; EnvironmentState = enum values [READY_FOR_DEPLOYMENT, DEPLOYING, ROLLING_BACK, ROLLED_BACK, REVERTED]. No enumValue override is modelled on either, so DeploymentState's wire token is the value itself; EnvironmentState's PascalCase wire form is recorded behavior only.

Bootstrap gate: get_bootstrap_status() reports pending == [] — appconfig and cloudwatch are both stood up in this checkout, so this is a fidelity finding, not a bootstrap gap.

Generated application/environment/profile/strategy ids and names, deployment numbers, role ARNs and the particular timeout at which the harness killed each check are per-run values; the requirement is the DeploymentState member -> wire-token mapping.

## Steps to Reproduce
Boot the emulator and point appconfig, appconfigdata, iam and cloudwatch clients at its data listener.
1. CreateApplication; CreateEnvironment twice — one with Monitors=[{AlarmArn: <alarm>, AlarmRoleArn: <role whose trust policy allows appconfig.amazonaws.com>}] and one with an unassumable AlarmRoleArn. Both roles carry an inline policy allowing cloudwatch:DescribeAlarms; PutMetricAlarm creates the monitored alarm.
2. CreateConfigurationProfile(LocationUri='hosted') + CreateHostedConfigurationVersion with a JSON payload.
3. CreateDeploymentStrategy(DeploymentDurationInMinutes=0, GrowthFactor=100, FinalBakeTimeInMinutes=1, ReplicateTo='NONE').
4. StartDeployment on the correctly-trusted environment with that strategy and version.
5. THE GRADED READ: GetDeployment(ApplicationId=<app>, EnvironmentId=<env>, DeploymentNumber=<n>) and inspect ['State'] until it is terminal.
   EXPECTED (AWS): 'COMPLETE' once the deployment settles (and 'BAKING' / 'VALIDATING' / 'DEPLOYING' while in flight, 'ROLLING_BACK' / 'ROLLED_BACK' / 'REVERTED' on the rollback paths) — a client waiting for State in ('COMPLETE','ROLLED_BACK','REVERTED') releases in seconds.
   OBSERVED: 'Complete', so that wait never matches and the client blocks until its own timeout.
6. Confirm the same value on StartDeployment's own response, StopDeployment, and ListDeployments items / DeploymentSummary.
7. Confirm GetEnvironment is UNCHANGED by the fix: it must keep returning 'ReadyForDeployment' (and 'Deploying' / 'RollingBack' while transitional), which it already does.
8. Re-replay appconfig__cloudwatch.alarm-monitor-role-not-assumable-deployment-rejected and appconfig__cloudwatch.cloudwatch-alarm-rollback-restores-previous-version. Both will then reach their graded legs — the unassumable-monitor-role deployment terminating ROLLED_BACK rather than COMPLETE, and the ALARM-during-bake deployment rolling back so the data plane serves the previous version again — which may surface further divergences behind this barrier.
