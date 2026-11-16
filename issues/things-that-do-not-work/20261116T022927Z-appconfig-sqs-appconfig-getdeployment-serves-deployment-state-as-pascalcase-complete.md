Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__sqs.deployment-events-to-sqs-extension-delivers-lifecycle-messages
Issue-Key: appconfig__sqs.deployment-events-to-sqs-extension-delivers-lifecycle-messages|_deploy_and_wait_complete#35|AssertionError|assert last is not None and last.get("State") == "COMPLETE", (
Issue-Commit: 29b39fea7962a85a6fc2f1b2b973f9d82128d2e7

# appconfig: GetDeployment serves Deployment$State as PascalCase 'Complete' instead of the modeled 'COMPLETE', so every wait-for-terminal poll never matches

> _Note: this issue was identified against the code base at commit `29b39fea7962a85a6fc2f1b2b973f9d82128d2e7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployment

## Description
Cause 1 of 1: GetDeployment (and every other response that carries Deployment$State) serves the enum value in PascalCase rather than the modeled wire value.

AWS: Smithy model services/service-com-amazonaws-appconfig-amazonappconfig/smithy-model/com-amazonaws-appconfig-amazonappconfig.json — GetDeployment output shape `Deployment`, member `State` targets `com.amazonaws.appconfig#DeploymentState`, an enum whose only values are `BAKING`, `VALIDATING`, `DEPLOYING`, `COMPLETE`, `ROLLING_BACK`, `ROLLED_BACK`, `REVERTED`. The wire value of a Smithy enum member is that SCREAMING_SNAKE string; AWS AppConfig serves `"State": "COMPLETE"` on a finished deployment. GetDeployment is `GET /applications/{ApplicationId}/environments/{EnvironmentId}/deployments/{DeploymentNumber}`, HTTP 200 — the call itself succeeds here, only the serialized enum value diverges.

Emulator: the response body carries `State` as a PascalCase, title-cased rendering of the enum member — `'Complete'` for a completed deployment (and by the same transform `'Deploying'`/`'RollingBack'` for the transitional states the check also names). The replayed check polls GetDeployment until `State` is one of the modeled terminal values and then asserts equality with `COMPLETE`; it timed out at the 300 s budget with `AssertionError: deployment 1 of version 1 did not reach COMPLETE within 300s; last state 'Complete'`. The deployment itself did reach a terminal state — only its serialized enum value is wrong.

Why: a client relying on AWS's behavior compares `State` against the SDK enum constant (`DeploymentState.COMPLETE`) or the literal `"COMPLETE"`. Against this emulator that comparison never becomes true, so every wait-for-terminal barrier hangs until its budget expires and every downstream leg of a deployment workflow (serving the deployed configuration, extension/lifecycle notifications, rollback assertions) is never reached. Typed SDKs that map unknown enum strings to an `Unknown`/sentinel variant lose the state entirely. The failure mode is a timeout, not an error, so it is indistinguishable from a stuck deployment.

Fix: the AppConfig deployment serializer/state projection for the `Deployment` output shape — the enum member must be emitted as its modeled wire value, not re-cased. The same transform must be audited for every other enum this service serves (DeploymentState, plus Environment$State and the DeploymentEvent enums), since the defect is in the rendering of enum members rather than in one field.

Note for the fixer: several open issues describe this same PascalCase enum rendering from other entry points — 20261110T191755Z-appconfig-getdeployment-getenvironment-serve-state-as-the-model.md, 20261115T213820Z-appconfig-lambda-2-net-new-causes-getdeployment-serves.md, 20261115T214126Z-appconfig-appconfigdata-getdeployment-serves-deployment-state-in-pascalcase.md, 20261115T214126Z-appconfigdata-appconfig-serves-deployment-state-in-pascalcase-complete-instead.md and 20261115T232640Z-appconfig-cloudwatch-getdeployment-serves-deployment-state-as-complete.md. None of them carries this check's replay key, so this check is filed on its own and should be re-replayed with the fix; it is very likely the same root cause.

Traces: appconfig__sqs.deployment-events-to-sqs-extension-delivers-lifecycle-messages.

## Evidence
Failing check: appconfig__sqs.deployment-events-to-sqs-extension-delivers-lifecycle-messages (verdict: mismatch).

Replay output:
  File "check_appconfig__sqs_deployment_events_to_sqs_extension_delivers_lifecycle_messages.py", line 333, in run
    dep1 = _deploy_and_wait_complete(ac, app_id, env_id, ds_id, cp_id, v1)
  File "check_appconfig__sqs_deployment_events_to_sqs_extension_delivers_lifecycle_messages.py", line 184, in _deploy_and_wait_complete
    assert last is not None and last.get("State") == "COMPLETE"
  AssertionError: deployment 1 of version 1 did not reach COMPLETE within 300s; last state 'Complete'

Expected (AWS, per the Smithy model): GetDeployment 200 with `State` one of BAKING | VALIDATING | DEPLOYING | COMPLETE | ROLLING_BACK | ROLLED_BACK | REVERTED — for a finished deployment, exactly `COMPLETE`.
Actual (emulator): GetDeployment 200 with `State` = `Complete` (PascalCase re-casing of the enum member). The variable parts of the check's message (deployment number 1, version 1, the 300 s budget) come from the request and the harness; the requirement is only that the served value be the modeled `COMPLETE`.

## Steps to Reproduce
1. Create an AppConfig application, environment, configuration profile and deployment strategy with a non-zero DeploymentDurationInMinutes.
2. Call StartDeployment and note the returned DeploymentNumber.
3. Poll GetDeployment(ApplicationId, EnvironmentId, DeploymentNumber) until the deployment finishes.
4. Read the `State` member of the response. AWS serves `COMPLETE`; the emulator serves `Complete`, so a comparison against the modeled enum value (or the SDK enum constant) never matches and the poll runs to its timeout.
