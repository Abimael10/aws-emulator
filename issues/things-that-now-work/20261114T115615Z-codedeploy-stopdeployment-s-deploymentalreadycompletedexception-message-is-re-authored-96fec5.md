Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy.lambda-deployment-stop-rolls-back-alias-and-refuses-restop
Issue-Shape-Id: com.amazonaws.codedeploy#CodeDeploy_20141006
Issue-Key: codedeploy.lambda-deployment-stop-rolls-back-alias-and-refuses-restop|run#30|AssertionError|assert done_message == f"The deployment {done_id} is already in state SUCCEEDED", (
Issue-Commit: 68f493cb4e7c78165f0ea5aca6e2cbc09f6e08cd

# CodeDeploy: StopDeployment's DeploymentAlreadyCompletedException message is re-authored for a SUCCEEDED deployment

> _Note: this issue was identified against the code base at commit `68f493cb4e7c78165f0ea5aca6e2cbc09f6e08cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopDeployment

## Description
One net-new root cause in the codedeploy surface, grounded in a failing behavioral check the replay report did NOT mark as already filed or owned.

Cause 1 of 1: StopDeployment refuses a terminal (SUCCEEDED) deployment with the correct error code but a re-authored message.
Failing check ids: codedeploy.lambda-deployment-stop-rolls-back-alias-and-refuses-restop.

AWS: Smithy (com.amazonaws.codedeploy#CodeDeploy_20141006) models StopDeployment with input {deploymentId (required), autoRollbackEnabled} and the client error DeploymentAlreadyCompletedException (errorKind client, HTTP 400, httpError defaulted). The model does not constrain the message text, so the message is recorded behavior only. The recording shows AWS refusing StopDeployment on a deployment that has already reached a terminal state with HTTP 400, error code DeploymentAlreadyCompletedException and this message template: fixed text "The deployment " + <deploymentId> + " is already in state " + <TERMINAL_STATUS_UPPERCASED>, with no trailing period. The only variable parts are the deploymentId taken from the StopDeployment request and the deployment's terminal status taken from the emulator's own state (SUCCEEDED here; FAILED / STOPPED for the other terminal states). No literal deployment id from any single run is part of the requirement.

Emulator: same HTTP status and same error code, but the message reads "The deployment <deploymentId> has already completed, current state SUCCEEDED." — different fixed text ("has already completed, current state " instead of " is already in state ") plus a trailing period AWS does not emit. Everything earlier in this check passed: the canary deployment was stopped, the alias rolled back to the prior version, repeated StopDeployment calls on the Stopped deployment were idempotent, and the AllAtOnce control deployment reached Succeeded — only the refusal message on the Succeeded deployment diverges.

Why: clients and SDK-based tooling that branch on, log, or surface the refusal reason compare or parse this sentence (error-message assertions in deployment orchestration, CloudWatch/log metric filters and alarm patterns, user-facing reporting). A re-authored sentence makes a correctly-refused StopDeployment look like a different refusal and breaks any matcher calibrated against AWS's wording, even though the code is right.

Fix: the StopDeployment handler's terminal-state refusal path — the place that constructs the DeploymentAlreadyCompletedException message — must emit AWS's template with the request's deploymentId and the stored terminal status. No state-model change is needed; the deployment record already holds the status it interpolates.

Related but NOT covering this: open issue 20261111T154845Z-codedeploy-stopdeployment-s-deploymentalreadycompletedexception-message-is-re-authored.md describes the same wording defect from the FAILED-state path and carries only the replay key of codedeploy.stop-deployment-already-completed-blocked (and codedeploy.alarm-rollback-returns-live-traffic-to-prior-version). It does not carry this check's key, so codedeploy.lambda-deployment-stop-rolls-back-alias-and-refuses-restop is not re-replayed by it; a fixer closing that issue should re-replay this check too, since the SUCCEEDED path must interpolate the stored terminal status the same way.

Traces: codedeploy.lambda-deployment-stop-rolls-back-alias-and-refuses-restop (Cause 1).

## Evidence
Cause 1 — codedeploy.lambda-deployment-stop-rolls-back-alias-and-refuses-restop, verdict mismatch. AssertionError at check line 185 (`assert done_message == f"The deployment {done_id} is already in state SUCCEEDED"`):
  expected: 'The deployment <deploymentId> is already in state SUCCEEDED'
  actual:   'The deployment d-CWASS0369 has already completed, current state SUCCEEDED.'
The assertion on the error code (`done_code == "DeploymentAlreadyCompletedException"`) passed, and HTTP 400 is the modelled/defaulted status, so status and code match; only the message text diverges.
Fixed text required by AWS: "The deployment " ... " is already in state " ... (no trailing period). Variable parts: the deploymentId from the StopDeployment request and the terminal status from the emulator's deployment record (SUCCEEDED in this run). The id d-CWASS0369 is a per-run value and not part of the requirement.
Earlier assertions in the same check all passed: StopDeployment on the in-flight TimeBasedCanary returned status 'Succeeded' with statusMessage "No more commands will be scheduled for execution in the deployment instances", the deployment reached 'Stopped' with errorInformation {code: MANUAL_STOP, message: "the deployment is cancelled due to an abort request"}, the alias was restored to the prior version with no residual AdditionalVersionWeights, two repeat StopDeployment calls were accepted idempotently without moving status or alias, and the AllAtOnce control deployment reached 'Succeeded'.

## Steps to Reproduce
1. lambda: create a function, publish two versions (v1, v2) and point an alias at v1.
2. codedeploy.CreateApplication(computePlatform="Lambda"); CreateDeploymentConfig with a TimeBasedCanary routing config; CreateDeploymentGroup with that config.
3. codedeploy.CreateDeployment on that group with deploymentConfigName="CodeDeployDefault.LambdaAllAtOnce" and an AppSpec revision moving the alias v1 -> v2.
4. Poll GetDeployment until deploymentInfo.status == "Succeeded".
5. Call StopDeployment(deploymentId=<that deployment>, autoRollbackEnabled=False).
   Expected: HTTP 400, code DeploymentAlreadyCompletedException, message "The deployment <deploymentId> is already in state SUCCEEDED".
   Actual: HTTP 400, code DeploymentAlreadyCompletedException, message "The deployment <deploymentId> has already completed, current state SUCCEEDED.".
6. GetDeployment again: status is still "Succeeded" (this part matches).
7. Replay codedeploy.lambda-deployment-stop-rolls-back-alias-and-refuses-restop.
