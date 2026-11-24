Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__appconfigdata.appconfig-hierarchy-round-trips-and-omits-validators appconfig__appconfigdata.completed-deployment-keeps-the-strategy-parameters-it-started-with appconfig__appconfigdata.configuration-profile-delete-before-its-versions-rejected appconfig__appconfigdata.hosted-configuration-version-is-served-byte-exactly appconfig__appconfigdata.identical-hosted-content-reuses-its-version-number appconfig__appconfigdata.logical-id-shaped-application-identifier-rejected appconfig__appconfigdata.redeploying-a-new-version-changes-the-bytes-served appconfig__appconfigdata.renamed-applications-old-name-keeps-resolving-to-its-original-owner-after-reassignment
Issue-Key: appconfig__appconfigdata.appconfig-hierarchy-round-trips-and-omits-validators|_poll_until#29|Timeout|time.sleep(interval_s)
Issue-Key: appconfig__appconfigdata.completed-deployment-keeps-the-strategy-parameters-it-started-with|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: appconfig__appconfigdata.configuration-profile-delete-before-its-versions-rejected|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: appconfig__appconfigdata.hosted-configuration-version-is-served-byte-exactly|_poll_until#9|Timeout|time.sleep(interval_s)
Issue-Key: appconfig__appconfigdata.identical-hosted-content-reuses-its-version-number|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Key: appconfig__appconfigdata.logical-id-shaped-application-identifier-rejected|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: appconfig__appconfigdata.redeploying-a-new-version-changes-the-bytes-served|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: appconfig__appconfigdata.renamed-applications-old-name-keeps-resolving-to-its-original-owner-after-reassignment|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Commit: 2b6dab747058248b284653e00ab7f0a2c4ef1311

# appconfig__appconfigdata: GetDeployment serves Deployment$State in PascalCase ('Complete') instead of the modeled 'COMPLETE', so the control-plane round-trip scenarios never see a settled deployment

> _Note: this issue was identified against the code base at commit `2b6dab747058248b284653e00ab7f0a2c4ef1311`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployment

## Description
One net-new root cause, observed through the appconfig__appconfigdata control-plane round-trip scenarios. All 8 failing checks under this heading are killed by the harness at 60.0s inside the same deployment-settle barrier and none of their graded assertions is reached.

NOT marked ALREADY FILED and NOT marked OWNED by any open issue in this replay. It is the SAME defect reported for the appconfigdata heading in the companion issue filed in this sweep (same cause, different service token, so it could not be bundled). Two prior issues describe this seam and must be reconciled rather than applied as written: fixed issue 20261030T085534Z-appconfig-serves-deployment-state-as-the-model-s.md changed the DeploymentState constants FROM the member names TO PascalCase, and open issue 20261110T191755Z-appconfig-getdeployment-getenvironment-serve-state-as-the-model.md asks for PascalCase again while describing the emulator as still serving the member names — a description that no longer matches this checkout. Neither carries any of this issue's replay keys.

=== Cause 1 of 1: Deployment$State is serialized in PascalCase, so a client's wait for a terminal DeploymentState never releases ===
Failing check ids: all 8 listed in trace_ids.

AWS: Smithy (services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json) models `Deployment$State` -> `com.amazonaws.appconfig#DeploymentState`, enum values [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED], with no @enumValue override, so the wire form is the member name; the AppConfig API Reference lists Deployment$State's valid values in the same SCREAMING_SNAKE form. The recorded behavior these checks encode agrees: each declares `_TERMINAL_DEPLOYMENT = ("COMPLETE", "ROLLED_BACK", "REVERTED")`, polls GetDeployment until State is in that set, and then asserts `State == "COMPLETE"`. The requirement is the mapping member -> wire token for every DeploymentState member, not one run's state.

Emulator: the DeploymentState wire tokens are hard-coded in PascalCase in services/service-com-amazonaws-appconfig-amazonappconfig/src/state.rs (`DEPLOYMENT_DEPLOYING = "Deploying"`, `DEPLOYMENT_BAKING = "Baking"`, `DEPLOYMENT_COMPLETE = "Complete"`, `DEPLOYMENT_ROLLING_BACK = "RollingBack"`, `DEPLOYMENT_ROLLED_BACK = "RolledBack"`, `DEPLOYMENT_REVERTED = "Reverted"`) and served verbatim: `Deployment::materialize` writes `DEPLOYMENT_COMPLETE` when the completion instant falls due and the Deployment serializer puts that string on the wire. GetDeployment answers `State: "Complete"` where AWS answers `State: "COMPLETE"`.

Why: State is the deployment's only completion signal. These scenarios each need a settled deployment before they can check what the data plane serves, what a completed deployment recorded of its strategy, or how a deletion/rename is gated — and so does any real deploy script, waiter or CI gate. Because no state the emulator can emit equals a modeled DeploymentState value, the wait never terminates: the client reports a failed deployment for a deployment that actually succeeded, and every behavior gated behind completion is unreachable.

Fix: the appconfig service's DeploymentState wire-value constants and the single boundary the enum crosses — the DEPLOYMENT_* constants in the appconfig service state module, every path that writes or compares a deployment state (deployment materialization, the in-flight/rollback/revert guards), and the Deployment / DeploymentSummary response serializers used by GetDeployment, StartDeployment, StopDeployment and ListDeployments; the same token must also be used wherever a state is interpolated into a refusal message. EnvironmentState is a different enum and is not disputed: these checks compare environment state against ('Deploying','RollingBack'), which the current PascalCase form satisfies, so leave it as it is.

Traces: the 8 check ids in trace_ids, each "Error: Check timed out after 60.0s" with no assertion graded. Generated ids and names, deployment numbers, timestamps and the 60.0s harness cap are per-run values, not requirements.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the booted emulator: 3/51 passed (5.9%), 48 mismatched, 0 errored, 0 skipped, 0 unloadable; bootstrap_signal false. Per-service: appconfig__appconfigdata 2/10 (20%), 8 failed.

All 8 failures report exactly:
  [mismatch] appconfig__appconfigdata.<check>
  Error: Check timed out after 60.0s

The barrier, from the check sources in the replay report:
  _TERMINAL_DEPLOYMENT = ("COMPLETE", "ROLLED_BACK", "REVERTED")
  ...
  number = ac.start_deployment(ApplicationId=..., EnvironmentId=..., DeploymentStrategyId=..., ConfigurationProfileId=..., ConfigurationVersion=str(version), Tags=...)["DeploymentNumber"]
  return _poll_until(
      lambda: ac.get_deployment(ApplicationId=app_id, EnvironmentId=env_id, DeploymentNumber=number),
      lambda d: d["State"] in _TERMINAL_DEPLOYMENT, ...)
  ...
  assert done["State"] == "COMPLETE", f"the deployment must complete, got {done['State']!r}"
The strategy is always a 0-minute duration / 100% growth / 0-minute bake one (CreateDeploymentStrategy(DeploymentDurationInMinutes=0, GrowthFactor=100, GrowthType='LINEAR', FinalBakeTimeInMinutes=0, ReplicateTo='NONE')), so completion is due at the instant the deployment starts.

Expected (AWS/Smithy): GetDeployment reports State "COMPLETE" within seconds; the _poll_until predicate releases and the check proceeds.
Actual (emulator): State is "Complete", so `d["State"] in ("COMPLETE","ROLLED_BACK","REVERTED")` is false forever; the check's own 180s/240s poll budget is still running when the harness kills it at 60.0s.

Emulator source (services/service-com-amazonaws-appconfig-amazonappconfig/src/state.rs):
  lines 70-82: `pub const DEPLOYMENT_DEPLOYING: &str = "Deploying";` ... `pub const DEPLOYMENT_COMPLETE: &str = "Complete";` ... `DEPLOYMENT_ROLLING_BACK = "RollingBack"`, `DEPLOYMENT_ROLLED_BACK = "RolledBack"`, `DEPLOYMENT_REVERTED = "Reverted"`, under a doc comment (lines 55-69) asserting the CamelCase form for this enum.
  line 3120: `self.state = DEPLOYMENT_COMPLETE.to_string();` in `Deployment::materialize` — the deployment does settle, it is only spelled wrongly.
  lines 100-111: the ENVIRONMENT_* PascalCase constants, which this issue does not dispute.
  services/service-com-amazonaws-appconfig-amazonappconfig/src/ops/start_deployment.rs line 765: `state: Some(stored.state.clone())` — the stored token is serialized unchanged.

Smithy consulted: DeploymentState enum values = [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED]; no @enumValue trait on its members. The StartDeployment handler's modeled-contract header repeats it: "`State` (DeploymentState, one of: BAKING | COMPLETE | DEPLOYING | REVERTED | ROLLED_BACK | ROLLING_BACK | VALIDATING)".

## Steps to Reproduce
Boot the emulator and point an appconfig client at its data listener.
1. CreateApplication(Name=<new>, Tags=...); CreateEnvironment(ApplicationId=<app>, Name=<new>); CreateConfigurationProfile(ApplicationId=<app>, Name=<new>, LocationUri='hosted'); CreateHostedConfigurationVersion(Content=b'{"useNewBackend": true}', ContentType='application/json').
2. CreateDeploymentStrategy(Name=<new>, DeploymentDurationInMinutes=0, GrowthFactor=100, GrowthType='LINEAR', FinalBakeTimeInMinutes=0, ReplicateTo='NONE').
3. StartDeployment(ApplicationId=<app>, EnvironmentId=<env>, DeploymentStrategyId=<ds>, ConfigurationProfileId=<cp>, ConfigurationVersion='1').
4. THE GRADED READ: GetDeployment(ApplicationId=<app>, EnvironmentId=<env>, DeploymentNumber=<n>)['State'].
   EXPECTED (AWS/Smithy): 'COMPLETE' once settled ('DEPLOYING'/'BAKING' while in flight).
   OBSERVED: 'Complete' — so a poll for a terminal DeploymentState value never releases.
5. Repeat for the other members and the other State-bearing responses: StartDeployment, StopDeployment (in-flight -> 'ROLLING_BACK' then 'ROLLED_BACK'; AllowRevert on a completed one -> 'REVERTED') and ListDeployments items; and confirm a refusal that names a state in its sentence uses the same token.
6. Verify GetEnvironment is unchanged and still answers 'ReadyForDeployment' — these checks' teardown compares environment state against ('Deploying','RollingBack').
7. Re-replay the 8 appconfig__appconfigdata check ids in trace_ids: each must clear its settle barrier in seconds and reach its own graded assertions.
