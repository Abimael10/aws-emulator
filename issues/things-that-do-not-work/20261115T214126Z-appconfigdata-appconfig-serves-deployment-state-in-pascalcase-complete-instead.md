Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfigdata.appconfigdata-binary-content-round-trip appconfigdata.appconfigdata-concurrent-token-reuse-all-succeed-idempotent appconfigdata.appconfigdata-configuration-unicode-content-round-trip appconfigdata.appconfigdata-content-type-round-trip-text-plain appconfigdata.appconfigdata-cross-application-environment-mismatch-not-found appconfigdata.appconfigdata-cross-application-profile-mismatch-not-found appconfigdata.appconfigdata-empty-configuration-profile-first-poll-returns-empty appconfigdata.appconfigdata-environment-configuration-isolation appconfigdata.appconfigdata-first-poll-returns-deployed-configuration appconfigdata.appconfigdata-initial-poll-reflects-deployment-after-session-start appconfigdata.appconfigdata-invalid-configuration-token-rejected appconfigdata.appconfigdata-new-deployment-reflected-on-next-poll appconfigdata.appconfigdata-next-poll-configuration-token-changes-each-call appconfigdata.appconfigdata-second-poll-no-change-returns-empty-configuration appconfigdata.appconfigdata-session-isolation-independent-tokens appconfigdata.appconfigdata-start-session-poll-interval-out-of-range-rejected appconfigdata.appconfigdata-version-label-round-trip appconfigdata.concurrent-independent-pollers-converge-on-new-deployment appconfigdata.concurrent-sessions-are-independent appconfigdata.corrupted-configuration-token-rejected-with-problem-detail appconfigdata.custom-content-type-preserved-exactly appconfigdata.early-poll-rejected-when-minimum-interval-requested appconfigdata.environment-deletion-does-not-immediately-break-active-session-or-new-session appconfigdata.feature-flags-profile-type-round-trip appconfigdata.hosted-version-violating-json-schema-validator-rejected appconfigdata.minimum-poll-interval-enforced-in-response appconfigdata.no-change-poll-preserves-content-type-and-version-label appconfigdata.poll-token-rotates-on-every-response appconfigdata.profile-deletion-does-not-immediately-break-active-session-token appconfigdata.profile-deployed-to-one-environment-not-another-rejected appconfigdata.same-named-profile-across-applications-does-not-cross-deliver appconfigdata.session-accepts-names-as-identifiers appconfigdata.session-token-scoped-to-configuration-profile appconfigdata.sibling-environment-deployment-does-not-leak-into-active-session appconfigdata.sibling-profile-deployment-does-not-affect-unrelated-session appconfigdata.start-session-missing-permission-access-denied appconfigdata.start-session-nonexistent-application-not-found appconfigdata.start-session-unknown-identifier-rejected-per-resource-type appconfigdata.version-label-absent-when-not-set-at-creation appconfigdata.version-label-surfaced-only-with-changed-content
Issue-Shape-Id: com.amazonaws.appconfigdata#AppConfigData
Issue-Key: appconfigdata.appconfigdata-binary-content-round-trip|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-concurrent-token-reuse-all-succeed-idempotent|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-configuration-unicode-content-round-trip|_deploy_and_wait#3|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-content-type-round-trip-text-plain|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-cross-application-environment-mismatch-not-found|_deploy_and_wait#3|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-cross-application-profile-mismatch-not-found|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-empty-configuration-profile-first-poll-returns-empty|_deploy_and_wait#9|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-environment-configuration-isolation|_deploy_and_wait#10|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-first-poll-returns-deployed-configuration|_deploy_and_wait#3|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-initial-poll-reflects-deployment-after-session-start|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-invalid-configuration-token-rejected|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-new-deployment-reflected-on-next-poll|_deploy_and_wait#3|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-next-poll-configuration-token-changes-each-call|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-second-poll-no-change-returns-empty-configuration|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-session-isolation-independent-tokens|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-start-session-poll-interval-out-of-range-rejected|_deploy_and_wait#1|Timeout|time.sleep(3)
Issue-Key: appconfigdata.appconfigdata-version-label-round-trip|_deploy_and_wait#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.concurrent-independent-pollers-converge-on-new-deployment|_deploy_version#6|Timeout|time.sleep(3)
Issue-Key: appconfigdata.concurrent-sessions-are-independent|_deploy_version#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.corrupted-configuration-token-rejected-with-problem-detail|_deploy_and_wait_complete#11|Timeout|time.sleep(1.0)
Issue-Key: appconfigdata.custom-content-type-preserved-exactly|_deploy_version#3|Timeout|time.sleep(3)
Issue-Key: appconfigdata.early-poll-rejected-when-minimum-interval-requested|_deploy_and_wait_complete#11|Timeout|time.sleep(1.0)
Issue-Key: appconfigdata.environment-deletion-does-not-immediately-break-active-session-or-new-session|_deploy_version#1|Timeout|time.sleep(3)
Issue-Key: appconfigdata.feature-flags-profile-type-round-trip|_deploy_version#9|Timeout|time.sleep(3)
Issue-Key: appconfigdata.hosted-version-violating-json-schema-validator-rejected|_deploy_version#18|Timeout|time.sleep(3)
Issue-Key: appconfigdata.minimum-poll-interval-enforced-in-response|_deploy_version#1|Timeout|time.sleep(3)
Issue-Key: appconfigdata.no-change-poll-preserves-content-type-and-version-label|_deploy_version#3|Timeout|time.sleep(3)
Issue-Key: appconfigdata.poll-token-rotates-on-every-response|_deploy_and_wait_complete#11|Timeout|time.sleep(1.0)
Issue-Key: appconfigdata.profile-deletion-does-not-immediately-break-active-session-token|_deploy_version#1|Timeout|time.sleep(3)
Issue-Key: appconfigdata.profile-deployed-to-one-environment-not-another-rejected|_deploy_version#7|Timeout|time.sleep(3)
Issue-Key: appconfigdata.same-named-profile-across-applications-does-not-cross-deliver|_deploy_version#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.session-accepts-names-as-identifiers|_deploy_and_wait_complete#14|Timeout|time.sleep(1.0)
Issue-Key: appconfigdata.session-token-scoped-to-configuration-profile|_deploy_and_wait_complete#15|Timeout|time.sleep(1.0)
Issue-Key: appconfigdata.sibling-environment-deployment-does-not-leak-into-active-session|_deploy_version#7|Timeout|time.sleep(3)
Issue-Key: appconfigdata.sibling-profile-deployment-does-not-affect-unrelated-session|_deploy_version#6|Timeout|time.sleep(3)
Issue-Key: appconfigdata.start-session-missing-permission-access-denied|_deploy_version#1|Timeout|time.sleep(3)
Issue-Key: appconfigdata.start-session-nonexistent-application-not-found|_deploy_version#1|Timeout|time.sleep(3)
Issue-Key: appconfigdata.start-session-unknown-identifier-rejected-per-resource-type|_deploy_and_wait_complete#11|Timeout|time.sleep(1.0)
Issue-Key: appconfigdata.version-label-absent-when-not-set-at-creation|_deploy_version#2|Timeout|time.sleep(3)
Issue-Key: appconfigdata.version-label-surfaced-only-with-changed-content|_deploy_and_wait_complete#18|Timeout|time.sleep(1.0)
Issue-Commit: 2b6dab747058248b284653e00ab7f0a2c4ef1311

# appconfig serves Deployment$State in PascalCase ('Complete') instead of AWS's SCREAMING_SNAKE wire value ('COMPLETE'), so every data-plane check's wait for a completed deployment never releases

> _Note: this issue was identified against the code base at commit `2b6dab747058248b284653e00ab7f0a2c4ef1311`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployment

## Description
One net-new root cause, reached through the appconfigdata data-plane checks. Every one of the 40 failing appconfigdata checks is killed by the harness at 60.0s inside the SAME setup barrier: it creates an application / environment / hosted configuration profile / zero-duration-zero-bake deployment strategy, calls StartDeployment, and then polls GetDeployment until `State` is terminal, with `_DEPLOYMENT_TERMINAL = ("COMPLETE", "ROLLED_BACK", "REVERTED")` or `while state != "COMPLETE"`. The emulator never produces any of those tokens, so the barrier can never release and no graded assertion in any of these 40 checks is ever reached.

NOT marked ALREADY FILED and NOT marked OWNED by any open issue in this replay. Two prior issues describe this same seam and must be reconciled rather than re-opened blindly: fixed issue 20261030T085534Z-appconfig-serves-deployment-state-as-the-model-s.md changed the DeploymentState constants FROM the member names TO PascalCase, and open issue 20261110T191755Z-appconfig-getdeployment-getenvironment-serve-state-as-the-model.md asks for PascalCase again while describing the emulator as still serving the member names (that description no longer matches the checkout). Neither carries any of this issue's replay keys. This issue reports the opposite direction for Deployment$State specifically, grounded in 40 checks that all demand the SCREAMING_SNAKE token; Environment$State is a DIFFERENT enum and is NOT disputed here (see Cause 1's Fix).

=== Cause 1 of 1: Deployment$State is serialized in PascalCase, so no client-visible deployment state ever equals a modeled DeploymentState value ===
Failing check ids: all 40 listed in trace_ids.

AWS: Smithy (services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json) models `Deployment$State` -> `com.amazonaws.appconfig#DeploymentState`, an enum whose values are exactly [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED]; there is no @enumValue override, so the wire form is the member name. The AppConfig API Reference lists Deployment$State's valid values in that same SCREAMING_SNAKE form, and the recorded behavior this suite is built on agrees: every one of the 40 checks waits on, or asserts equality with, the literal `"COMPLETE"` and treats `"ROLLED_BACK"` / `"REVERTED"` as the other terminal values. The requirement is the MAPPING member -> wire token for every member of DeploymentState, not any one run's state; which member is reported (a 0-minute duration / 0-minute bake strategy settling at the completion member) is already correct.

Emulator: the DeploymentState wire tokens are hard-coded in PascalCase and stored and served verbatim — services/service-com-amazonaws-appconfig-amazonappconfig/src/state.rs declares `DEPLOYMENT_DEPLOYING = "Deploying"`, `DEPLOYMENT_BAKING = "Baking"`, `DEPLOYMENT_COMPLETE = "Complete"`, `DEPLOYMENT_ROLLING_BACK = "RollingBack"`, `DEPLOYMENT_ROLLED_BACK = "RolledBack"`, `DEPLOYMENT_REVERTED = "Reverted"`, with a doc comment asserting that AppConfig serves this enum CamelCase. `Deployment::materialize` writes `self.state = DEPLOYMENT_COMPLETE.to_string()` when the completion instant falls due, and the Deployment/DeploymentSummary serializers report that string. So GetDeployment (and StartDeployment, StopDeployment and ListDeployments items) answer `State: "Complete"` where AWS answers `State: "COMPLETE"`.

Why: `State` is the only completion signal AppConfig offers for a deployment. A client that waits for a release to settle — a deploy script, a CLI/CDK waiter, a CI gate, a Step Functions poller, or (here) every AppConfigData check that must have a deployed configuration before it can start a configuration session — compares `State` against the modeled value. Against this emulator that comparison is false for every state, so the client hangs until its own timeout and then reports a failure for a deployment that in fact succeeded; everything gated behind a settled deployment becomes unreachable. The same mis-cased token also leaks anywhere a state is interpolated into a refusal message.

Fix: the appconfig service's DeploymentState wire-value constants and the one place the enum crosses the wire — the DEPLOYMENT_* constants in the appconfig service state module, every path that writes or compares a deployment state (the deployment materialization, the in-flight/rollback/revert guards) and the Deployment / DeploymentSummary response serializers. Deployment$State must carry the modeled member name; EnvironmentState is a separate enum that this suite's checks accept in its current PascalCase form (their cleanup compares environment state against 'Deploying'/'RollingBack'), so do NOT change it while fixing this, and reconcile open issue 20261110T191755Z before acting on its PascalCase request for Deployment$State.

Traces: the 40 appconfigdata check ids in trace_ids, each reported as "Error: Check timed out after 60.0s" with no assertion graded. Generated application/environment/profile/strategy ids and names, deployment numbers, timestamps and the particular 60.0s cap the harness killed each check at are per-run values and are not part of the requirement.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the booted emulator: 3/51 passed (5.9%), 48 mismatched, 0 errored, 0 skipped, 0 unloadable; bootstrap_signal false (the emulator answered throughout — this is a fidelity finding, not a bootstrap gap). Per-service: appconfigdata 1/41 (2%), 40 failed; appconfig__appconfigdata 2/10 (20%), 8 failed.

Every one of the 40 appconfigdata failures reports exactly:
  [mismatch] appconfigdata.<check>
  Error: Check timed out after 60.0s
No assertion is reached, which is itself the finding: each check dies inside its setup barrier, before the graded data-plane call is issued.

The barrier, from the check sources in the replay report (two equivalent spellings appear across the 40):
  _DEPLOYMENT_TERMINAL = ("COMPLETE", "ROLLED_BACK", "REVERTED")
  ...
  number = ac.start_deployment(ApplicationId=..., EnvironmentId=..., DeploymentStrategyId=..., ConfigurationProfileId=..., ConfigurationVersion=...)["DeploymentNumber"]
  while time.monotonic() < deadline:
      final = ac.get_deployment(ApplicationId=..., EnvironmentId=..., DeploymentNumber=number)
      if final.get("State") in _DEPLOYMENT_TERMINAL:
          assert final["State"] == "COMPLETE", f"deployment {number} settled at {final['State']!r}, expected 'COMPLETE'"
          return final
      time.sleep(1.0)
  raise AssertionError(f"deployment {number} never reached a terminal state within {timeout_s}s; ...")
and the simpler form `while state != "COMPLETE" and time.monotonic() < deadline: ... state = ac.get_deployment(...)["State"]`. The setup strategy is always CreateDeploymentStrategy(DeploymentDurationInMinutes=0, GrowthFactor=100, FinalBakeTimeInMinutes=0, ReplicateTo='NONE'), i.e. the deployment is due to complete at the instant it starts; the check source itself documents why the wait is mandatory: "GetLatestConfiguration serves nothing at all until a deployment has completed (StartConfigurationSession itself is refused with ResourceNotFoundException), so the wait is the precondition of the whole check."

Expected (AWS / Smithy): GetDeployment reports State "COMPLETE" within seconds for a 0-minute/0-bake strategy (and "DEPLOYING"/"BAKING" in flight, "ROLLING_BACK"/"ROLLED_BACK"/"REVERTED" for the other members).
Actual (emulator): State is the PascalCase token "Complete" (and "Deploying"/"Baking"/"RollingBack"/"RolledBack"/"Reverted"), so the predicate is never satisfied, each check's own 120-180s wait is still running when the harness's 60.0s per-check cap kills it.

Emulator source (services/service-com-amazonaws-appconfig-amazonappconfig/src/state.rs):
  lines 70-82: `pub const DEPLOYMENT_DEPLOYING: &str = "Deploying";` / `DEPLOYMENT_BAKING = "Baking"` / `DEPLOYMENT_COMPLETE = "Complete"` / `DEPLOYMENT_ROLLING_BACK = "RollingBack"` / `DEPLOYMENT_ROLLED_BACK = "RolledBack"` / `DEPLOYMENT_REVERTED = "Reverted"`, under a doc comment (lines 55-69) claiming "AppConfig serves Deployment$State in the CamelCase form".
  line 3120: `self.state = DEPLOYMENT_COMPLETE.to_string();` inside `Deployment::materialize` — the single place the completion state is written; it is reached because `completion_due` for a 0/0 strategy equals `started_at`, so the deployment IS settled, only mis-spelled.
  lines 100-111: the ENVIRONMENT_* constants, PascalCase ("ReadyForDeployment", "Deploying", ...) — left alone by this issue.
  services/service-com-amazonaws-appconfig-amazonappconfig/src/ops/start_deployment.rs line 765: `state: Some(stored.state.clone())` — the serializer puts the stored token on the wire unchanged.

Smithy consulted: DeploymentState enum values = [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED]; EnvironmentState enum values = [READY_FOR_DEPLOYMENT, DEPLOYING, ROLLING_BACK, ROLLED_BACK, REVERTED]. No @enumValue trait on either enum's members. The StartDeployment handler's own modeled-contract header repeats it: "`State` (DeploymentState, one of: BAKING | COMPLETE | DEPLOYING | REVERTED | ROLLED_BACK | ROLLING_BACK | VALIDATING)".

## Steps to Reproduce
Boot the emulator and point appconfig + appconfigdata clients at its data listener (region/credentials irrelevant).
1. CreateApplication(Name=<new>); CreateEnvironment(ApplicationId=<app>, Name=<new>).
2. CreateConfigurationProfile(ApplicationId=<app>, Name=<new>, LocationUri='hosted'); CreateHostedConfigurationVersion(Content=b'{"identifier":"present"}', ContentType='application/json').
3. CreateDeploymentStrategy(DeploymentDurationInMinutes=0, GrowthFactor=100, FinalBakeTimeInMinutes=0, ReplicateTo='NONE'); StartDeployment with that strategy, profile and version.
4. THE GRADED READ: GetDeployment(ApplicationId=<app>, EnvironmentId=<env>, DeploymentNumber=<n>) and inspect ['State'].
   EXPECTED (AWS/Smithy): the modeled member name — 'COMPLETE' once settled (and 'DEPLOYING'/'BAKING' while in flight).
   OBSERVED: 'Complete', so any client comparing against 'COMPLETE' never matches and polls until its own timeout.
5. Confirm the same mapping on every other State-bearing deployment response: StartDeployment, StopDeployment, and ListDeployments items. StopDeployment of an in-flight deployment must report 'ROLLING_BACK' then 'ROLLED_BACK'; StopDeployment(AllowRevert=true) of a completed one must report 'REVERTED'.
6. Confirm the inverse where a state is interpolated into a refusal message (e.g. StopDeployment on a settled deployment): the sentence must name the same token the response carries.
7. Leave GetEnvironment alone and verify it still answers 'ReadyForDeployment': the appconfigdata checks' cleanup compares environment state against ('Deploying','RollingBack'), so the environment enum is not what blocks them.
8. Re-replay the 40 appconfigdata check ids in trace_ids. Each must get past its setup barrier in seconds and reach its own graded data-plane assertions; failures after that point are separate causes, not this one.
