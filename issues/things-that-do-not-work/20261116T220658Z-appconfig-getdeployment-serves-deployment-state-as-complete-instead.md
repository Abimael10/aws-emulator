Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig.start-experiment-run-invalid-treatment-allocation-rejected appconfig.stop-experiment-run-double-stop-conflict
Issue-Shape-Id: com.amazonaws.appconfig#AmazonAppConfig
Issue-Key: appconfig.start-experiment-run-invalid-treatment-allocation-rejected|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Key: appconfig.stop-experiment-run-double-stop-conflict|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Commit: fe23c86fdd90a5fdafcd9dec1c5ca339fb33a9c4

# appconfig: GetDeployment serves Deployment$State as 'COMPLETE' instead of the recorded CamelCase 'Complete', so a client's wait for a settled deployment never converges and both experiment-run lifecycle scenarios exhaust their budget

> _Note: this issue was identified against the code base at commit `fe23c86fdd90a5fdafcd9dec1c5ca339fb33a9c4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployment

## Description
Cause 1 of 1: Deployment$State is served in SCREAMING_SNAKE form on the GetDeployment read path, so no client that waits for the CamelCase value AWS puts on the wire ever observes a settled deployment. Failing checks: appconfig.start-experiment-run-invalid-treatment-allocation-rejected, appconfig.stop-experiment-run-double-stop-conflict.

AWS: recorded behavior only for the CASING. Smithy: enum DeploymentState = BAKING | VALIDATING | DEPLOYING | COMPLETE | ROLLING_BACK | ROLLED_BACK | REVERTED, and the shipped model carries no @enumValue trait on those members, so a model-only reading would put the member name on the wire. The recording contradicts that: both failing checks wait on GetDeployment's Deployment$State for the literal 'Complete' and state in-line that the real wire values are CamelCase ('Complete', and 'ReadyForDeployment' for the sibling Environment$State) and that polling for the documented SCREAMING_SNAKE spelling 'never matches'. The sibling Environment$State in this same service is already served in that CamelCase form by the emulator, so the two enums are being spelled by two different rules. The requirement here is a RESPONSE MEMBER VALUE, not a message: the stable part is the state vocabulary (Complete / Deploying / Baking / Validating / RollingBack / RolledBack / Reverted); which one a given read returns comes from the deployment's own position in its strategy's schedule, never from a recorded run.

Emulator: state.rs declares DEPLOYMENT_COMPLETE = "COMPLETE" / DEPLOYMENT_DEPLOYING = "DEPLOYING" (and the rest as member names), with a doc comment asserting that the member name IS the served form for this enum, while ENVIRONMENT_READY_FOR_DEPLOYMENT = "ReadyForDeployment" keeps the CamelCase form for the environment. A zero-duration / zero-bake custom strategy is answered already settled (the 'paces no stage' path), so the deployment IS terminal immediately — it just reports 'COMPLETE'. Both checks therefore spin their GetDeployment settle barrier for its full 60s window without a single match and are killed by the harness: 'Check timed out after 60.0s'. Neither check ever reaches the behavior it was written to exercise (StartExperimentRun's BadRequestException for an undefined treatment name, and StopExperimentRun's BadRequestException on a double stop), so the whole experiment-run lifecycle is unexercised behind this one value.

Why: every real client settles a deployment by comparing Deployment$State to the value AWS serves — SDK waiters, CloudFormation-style polling loops and hand-written wait loops all do this. Against the emulator such a loop never terminates: the deployment is finished, reports a spelling the client does not recognise, and the caller hangs until its own timeout and then reports a failure that never happened. Anything gated behind 'the deployment has settled' (starting an experiment run, a second deployment into the environment, a data-plane read of the deployed version) is unreachable in an automated test.

Fix: the AppConfig state module's DeploymentState vocabulary and whatever serves Deployment$State out of it — the constants in the service's state (DEPLOYMENT_* ) plus the GetDeployment / StartDeployment / StopDeployment / ListDeployments serializers that copy the stored string onto the wire — must spell this enum the same recorded way Environment$State is already spelled, with the refusal-sentence path that needs the MEMBER name going through the existing state_member_name() mapping rather than relying on stored == member. NOTE for the fixer: open issues 20261116T072120Z-appconfig-serves-deployment-state-in-pascalcase-complete-deploying.md, 20261116T014021Z-appconfig-serves-deployment-state-in-pascalcase-across-the.md and 20261115T214126Z-appconfig-appconfigdata-getdeployment-serves-deployment-state-in-pascalcase.md assert the OPPOSITE direction for this same member and do not cover these two checks (the current build serves 'COMPLETE'); reconcile them against the recorded wire form these two checks require instead of flipping the casing back and forth.

Traces: appconfig.start-experiment-run-invalid-treatment-allocation-rejected and appconfig.stop-experiment-run-double-stop-conflict, both of which die inside their GetDeployment settle barrier. Emulator citations: services/service-com-amazonaws-appconfig-amazonappconfig/src/state.rs lines 58-114 (the two state vocabularies), lines 1962-1993 (Environment::materialize / owed_state), lines 3113-3150 (Deployment::materialize), lines 3803-3820 (state_member_name).

## Evidence
Expected (recorded AWS, as both checks encode it): GetDeployment on a deployment made with DeploymentDurationInMinutes=0 / GrowthFactor=100 / FinalBakeTimeInMinutes=0 answers State == 'Complete' (CamelCase), matching the sibling GetEnvironment State == 'ReadyForDeployment'.

Actual (emulator): GetDeployment answers State == 'COMPLETE'. services/service-com-amazonaws-appconfig-amazonappconfig/src/state.rs:74-86 defines DEPLOYMENT_DEPLOYING="DEPLOYING", DEPLOYMENT_BAKING="BAKING", DEPLOYMENT_COMPLETE="COMPLETE", DEPLOYMENT_ROLLING_BACK="ROLLING_BACK", DEPLOYMENT_ROLLED_BACK="ROLLED_BACK", DEPLOYMENT_REVERTED="REVERTED"; state.rs:105-113 defines ENVIRONMENT_READY_FOR_DEPLOYMENT="ReadyForDeployment", ENVIRONMENT_DEPLOYING="Deploying". So the environment half of the wait matches and the deployment half can never match.

Consequence observed in the replay: both experiment checks report 'Error: Check timed out after 60.0s' — the deployment settle poll (timeout_s=60.0, interval_s=2.0) consumes the entire check budget, and the assertions that follow it (StartExperimentRun BadRequestException 'Invalid treatment name: ...', list_experiment_runs == [], GetExperimentDefinition Status == 'IDLE'; StopExperimentRun first-stop DONE then BadRequestException on the repeat) are never reached.

Smithy (model_path services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json): shape DeploymentState values [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED], no @enumValue; shape EnvironmentState values [READY_FOR_DEPLOYMENT, DEPLOYING, ROLLING_BACK, ROLLED_BACK, REVERTED], no @enumValue — i.e. the model gives the two enums identical treatment while the recording and the emulator both treat Environment$State as CamelCase, which is the evidence that the recorded wire form, not the member name, governs Deployment$State too.

## Steps to Reproduce
1. Point an SDK at the emulator and create an AppConfig application, environment and a hosted (LocationUri='hosted', Type='AWS.AppConfig.FeatureFlags') configuration profile, then CreateHostedConfigurationVersion with a feature-flag document.
2. CreateDeploymentStrategy with DeploymentDurationInMinutes=0, GrowthFactor=100, FinalBakeTimeInMinutes=0, ReplicateTo='NONE'.
3. StartDeployment of that version into the environment with that strategy.
4. Call GetDeployment(ApplicationId, EnvironmentId, DeploymentNumber) and read State: the emulator answers 'COMPLETE'; AWS answers 'Complete'.
5. Poll GetDeployment until State == 'Complete' (what the two failing checks do, 60s budget): the loop never matches and the check is killed at its deadline, so StartExperimentRun / StopExperimentRun are never reached.
6. For contrast, GetEnvironment on the same environment answers State == 'ReadyForDeployment' (CamelCase) and that half of the wait succeeds.
