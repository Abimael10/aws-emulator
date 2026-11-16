Issue-Source: trace-sweep-divergence
Issue-Trace: appconfig-deployment-stop-rollback
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appconfig-deployment-stop-rollback appconfig-end-to-end-deployment-workflow appconfig-experiment-run-full-lifecycle appconfig-experiment-run-lifecycle appconfig-experiment-run-multiple-runs appconfig-experiment-run-treatment-overrides appconfig-lacs-traj-allatonce-bake-blocks-redeploy-stop-rolls-back-value appconfig-lacs-traj-deprecated-getconfiguration-logical-ids-then-names appconfig-lacs-traj-redeploy-parameter-mistakes-then-profile-name-accepted
Issue-Shape-Id: com.amazonaws.appconfig#AmazonAppConfig
Issue-Key: appconfig|StartDeployment|2xx->2xx|$.State
Issue-Key: appconfig|StopDeployment|2xx->2xx|$.State
Issue-Key: appconfig|ListDeployments|2xx->2xx|$.Items[*]
Issue-Key: appconfig|ListDeployments|2xx->2xx|$.Items[*].State
Issue-Commit: 78a28d66acabd31f329494a20428d6e048dfa41c

# appconfig serves Deployment State in PascalCase across the deployment write and list paths instead of AWS's SCREAMING_SNAKE enum value (4 causes)

> _Note: this issue was identified against the code base at commit `78a28d66acabd31f329494a20428d6e048dfa41c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartDeployment

## Description
This issue bundles 4 net-new causes, all in appconfig, that share ONE root cause: the value the emulator puts on the wire for Deployment$State / DeploymentSummary$State is a PascalCase rendering of its internal state name ('Deploying', 'RolledBack', 'Complete') instead of the modeled DeploymentState enum value AWS serves ('DEPLOYING', 'ROLLED_BACK', 'COMPLETE'). The defect reaches every operation that projects a deployment: StartDeployment (201), StopDeployment (202) and ListDeployments (200). A fixer must close all FOUR causes below, not just the first.

Related open issues that do NOT cover these clusters (the replay reports owned_by empty for all four): 20261110T191755Z-appconfig-getdeployment-getenvironment-serve-state-as-the-model.md, 20261115T213820Z-appconfig-lambda-2-net-new-causes-getdeployment-serves.md, 20261115T214126Z-appconfig-appconfigdata-getdeployment-serves-deployment-state-in-pascalcase.md, 20261115T214126Z-appconfigdata-appconfig-serves-deployment-state-in-pascalcase-complete-instead.md and 20261115T232640Z-appconfig-cloudwatch-getdeployment-serves-deployment-state-as-complete.md all describe the SAME casing fault on the GetDeployment (and GetEnvironment) READ path only; 20261112T075825Z-appconfig-stopdeployment-refuses-a-baking-deployment-so-the.md owns the GetDeployment casing cluster of this replay. None of them binds the StartDeployment, StopDeployment or ListDeployments steps filed here, so those write/list projections stay unfixed if only the read path is corrected.

Cause 1 of 4: StartDeployment's 201 body reports State 'Deploying' instead of 'DEPLOYING'.
  AWS: Smithy: StartDeployment POST /applications/{ApplicationId}/environments/{EnvironmentId}/deployments, http code 201, output shape Deployment; Deployment$State targets com.amazonaws.appconfig#DeploymentState, an enum whose only values are BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED. The wire value of an enum member IS that value string, so a freshly accepted deployment that is rolling out is serialized as 'DEPLOYING'; the recorded response confirms 'DEPLOYING'.
  Emulator: the same POST answers 201 (status matches) but serializes $.State as 'Deploying' — the internal state name converted to PascalCase, which is not a member of DeploymentState.
  Why: a typed SDK client deserializes State into the DeploymentState enum; an unmodeled string falls into the SDK's unknown-variant bucket, so a caller that switches on DeploymentState::Deploying (or compares against the literal 'DEPLOYING') never matches, and the standard start-then-poll-until-terminal loop cannot tell a rolling-out deployment from an unrecognized one. Clients that branch on the value take the default/error path for an otherwise successful deployment.
  Fix: the serializer for Deployment / DeploymentSummary in the appconfig response-projection layer (or the Display/Serialize impl of the internal deployment-state type it reuses) — every DeploymentState must be emitted as its modeled enum value, so the internal variant naming must be mapped to the enum's wire token rather than reused verbatim.
  Traces: appconfig-deployment-stop-rollback#task#6, plus 11 further steps in the same cluster (appconfig-end-to-end-deployment-workflow#task#6, appconfig-experiment-run-full-lifecycle#setup#6, appconfig-experiment-run-lifecycle#task#6, appconfig-experiment-run-multiple-runs#setup#6, appconfig-experiment-run-treatment-overrides#setup#6, appconfig-lacs-traj-allatonce-bake-blocks-redeploy-stop-rolls-back-value#setup#6 / task#10 / task#16, appconfig-lacs-traj-deprecated-getconfiguration-logical-ids-then-names#setup#6, appconfig-lacs-traj-redeploy-parameter-mistakes-then-profile-name-accepted#setup#6 / task#10).

Cause 2 of 4: StopDeployment's 202 body reports State 'RolledBack' instead of 'ROLLED_BACK'.
  AWS: Smithy: StopDeployment DELETE /applications/{ApplicationId}/environments/{EnvironmentId}/deployments/{DeploymentNumber}, http code 202, output Deployment; State -> DeploymentState, whose terminal rollback member is ROLLED_BACK. The recorded 202 carries State 'ROLLED_BACK'.
  Emulator: answers 202 (status matches) with $.State 'RolledBack' — the same PascalCase rendering, here collapsing the enum's underscore as well, so the multi-word members are doubly unrecoverable.
  Why: ROLLED_BACK is the value a client waits for to confirm an abort actually took effect. Receiving 'RolledBack' means the rollback confirmation never matches, so safe-rollback tooling either times out waiting for a terminal state or reports the stop as failed although the emulator performed it.
  Fix: the same Deployment serializer/state-token mapping as Cause 1; multi-word members (ROLLED_BACK, ROLLING_BACK) must keep the enum's underscore form rather than being camel-cased.
  Traces: appconfig-deployment-stop-rollback#task#8, and appconfig-lacs-traj-allatonce-bake-blocks-redeploy-stop-rolls-back-value#task#13 in the same cluster.

Cause 3 of 4: ListDeployments' summary items carry State 'Complete', so the whole DeploymentSummary fails to match.
  AWS: Smithy: ListDeployments GET /applications/{ApplicationId}/environments/{EnvironmentId}/deployments, http code 200, output Deployments{Items -> DeploymentList of DeploymentSummary}; DeploymentSummary$State -> DeploymentState, documented 'The state of the deployment'. A finished deployment's summary therefore reports State 'COMPLETE' alongside DeploymentNumber, ConfigurationProfileId, ConfigurationName, ConfigurationVersion, DeploymentDurationInMinutes, GrowthType, GrowthFactor, FinalBakeTimeInMinutes, PercentageComplete, StartedAt, CompletedAt and Type.
  Emulator: answers 200 with every other summary member agreeing, but $.Items[0].State is 'Complete', which makes the recorded summary object compare as missing/non-matching as a whole.
  Why: a client listing an environment's deployment history to find the one that is finished (or still in flight) filters on State; with an unmodeled token every item falls through the filter, so the history reads as containing no completed deployment at all. The divergence is in DeploymentSummary, a different shape from Deployment, so fixing only the Deployment serializer leaves this path broken.
  Fix: the DeploymentSummary projection used by ListDeployments must use the same DeploymentState wire mapping as Cause 1 (the generated/shared state token), not a PascalCase rendering of the stored value.
  Traces: appconfig-end-to-end-deployment-workflow#task#9.

Cause 4 of 4: the same ListDeployments casing on every item of a multi-deployment page.
  AWS: as Cause 3 — each DeploymentSummary in Items reports its own DeploymentState value; a page of two finished deployments reports 'COMPLETE' for both.
  Emulator: $.Items[0].State and $.Items[1].State are both 'Complete'; status matches at 200.
  Why: confirms the defect is per-item in the list serializer rather than a one-off on the newest deployment — a client paging an environment's history mis-reads every entry, so no element of the history can be classified.
  Fix: same as Cause 3 (DeploymentSummary state serialization), verified across a page with more than one item.
  Traces: appconfig-lacs-traj-redeploy-parameter-mistakes-then-profile-name-accepted#task#13.

## Evidence
Replay of trace set simparity-wire-aws-v1: 47 fixtures, 570 steps, 551 passed, 19 mismatched, score 96.7%. All four clusters below are reported with stub_path empty and owned_by empty.

Cause 1 — appconfig-deployment-stop-rollback#task#6, StartDeployment, expected status 201 / actual 201 (mismatch): $.State value_mismatch — expected 'DEPLOYING', got 'Deploying'. Cluster affected_count 12.

Cause 2 — appconfig-deployment-stop-rollback#task#8, StopDeployment, expected 202 / actual 202: $.State value_mismatch — expected 'ROLLED_BACK', got 'RolledBack'. Cluster affected_count 2.

Cause 3 — appconfig-end-to-end-deployment-workflow#task#9, ListDeployments, expected 200 / actual 200: $.Items[0] missing — expected a DeploymentSummary with State 'COMPLETE' (DeploymentNumber 1, GrowthType 'LINEAR', GrowthFactor 100.0, PercentageComplete 100.0, Type 'USER', DeploymentDurationInMinutes 0, FinalBakeTimeInMinutes 0, StartedAt/CompletedAt not asserted), got the same object with State 'Complete'. State is the only diverging member; the profile id, configuration name and timestamps in the recording are per-run values, not requirements.

Cause 4 — appconfig-lacs-traj-redeploy-parameter-mistakes-then-profile-name-accepted#task#13, ListDeployments, expected 200 / actual 200: $.Items[0].State value_mismatch — expected 'COMPLETE', got 'Complete'; $.Items[1].State value_mismatch — expected 'COMPLETE', got 'Complete'.

Model basis: com.amazonaws.appconfig#DeploymentState is an enum with values [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED]; Deployment$State (StartDeployment/StopDeployment/GetDeployment output) and DeploymentSummary$State (ListDeployments output) both target it. None of 'Deploying', 'RolledBack' or 'Complete' is a member.

## Steps to Reproduce
1. Create an AppConfig application, an environment, and a hosted configuration profile with at least one configuration version.
2. Call StartDeployment for that version and read the 201 body's State: AWS reports 'DEPLOYING', the emulator reports 'Deploying' (Cause 1). Replay appconfig-deployment-stop-rollback#task#6.
3. Call StopDeployment on that deployment number and read the 202 body's State: AWS reports 'ROLLED_BACK', the emulator reports 'RolledBack' (Cause 2). Replay appconfig-deployment-stop-rollback#task#8.
4. Let a deployment finish, then call ListDeployments for the environment and read $.Items[*].State: AWS reports 'COMPLETE' on every summary, the emulator reports 'Complete' (Causes 3 and 4). Replay appconfig-end-to-end-deployment-workflow#task#9 and appconfig-lacs-traj-redeploy-parameter-mistakes-then-profile-name-accepted#task#13 (a two-item page).
5. Deserialize any of those responses with a generated SDK client and observe State landing in the enum's unknown-variant bucket instead of DeploymentState::Deploying / RolledBack / Complete.
