Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__lambda.at-deployment-tick-directive-rollback-reverts-the-deployment appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected
Issue-Key: appconfig__lambda.at-deployment-tick-directive-rollback-reverts-the-deployment|_wait_terminal#43|AssertionError|raise AssertionError(
Issue-Key: appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content|_deploy_and_wait_complete#59|Timeout|last = ac.get_deployment(
Issue-Key: appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected|_deploy_to_complete#33|Timeout|time.sleep(2.0)
Issue-Commit: b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d

# appconfig__lambda: 2 net-new causes — GetDeployment serves Deployment.State in PascalCase instead of the modeled enum value, and the hosted-version / validator extension action points never settle

> _Note: this issue was identified against the code base at commit `b36f75f4e986e022d916cbe82a6e6b2cbd0bc56d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployment

## Description
Two net-new root causes, both grounded in failing checks of the appconfig__lambda suite group; neither is marked ALREADY FILED or OWNED in the replay report.

Cause 1 of 2: GetDeployment reports Deployment.State as `Complete` where the API's enum value is `COMPLETE`, so no client waiting on a terminal deployment state ever sees one.
AWS: AppConfig's DeploymentState is an enumerated wire value. Smithy: com.amazonaws.appconfig#DeploymentState values are exactly BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED; GetDeployment/ListDeployments report State from that set, and a deployment that finishes successfully reports COMPLETE at PercentageComplete 100.0.
Emulator: GetDeployment answers State `Complete` (PascalCase) with PercentageComplete 100.0 — a value that is not a member of the modeled enum. The fixed requirement is the enum value itself; the particular deployment number and the percentage are per-run values.
Why: the normal AppConfig client pattern is to poll GetDeployment until State is one of the terminal enum values. Against this emulator that poll never terminates: an SDK-generated enum deserializer sees an unknown value, and a hand-written comparison against COMPLETE/ROLLED_BACK/REVERTED never matches, so the caller waits out its whole timeout on a deployment that in fact finished. Everything gated on a terminal deployment — rollback assertions, the StopDeployment-is-terminal refusal, the next deployment of the same environment — becomes unreachable.
Fix: the GetDeployment/ListDeployments serializer and the deployment state store, which must emit the DeploymentState enum values rather than a PascalCase rendering of them. NOTE for the fixer: open issue 20261110T191755Z-appconfig-getdeployment-getenvironment-serve-state-as-the-model.md asserts the OPPOSITE direction for this same member (that AWS's wire value is PascalCase and SCREAMING_SNAKE is the defect), as does the resolved 20261030T085534Z-appconfig-serves-deployment-state-as-the-model-s.md; this cause is grounded in the Smithy enum values quoted above and in the failing check, and the two must be reconciled rather than flipped back and forth. Neither of those issues carries this replay key.
Traces: appconfig__lambda.at-deployment-tick-directive-rollback-reverts-the-deployment.

Cause 2 of 2: the PRE_CREATE_HOSTED_CONFIGURATION_VERSION action point and the configuration profile's LAMBDA validator never settle, so neither extension scenario reaches a verdict.
AWS: an extension associated to a configuration profile at PRE_CREATE_HOSTED_CONFIGURATION_VERSION is invoked synchronously by CreateHostedConfigurationVersion and may transform the content that is then stored, so a subsequent GetHostedConfigurationVersion returns the TRANSFORMED bytes; and a profile whose Validators include a LAMBDA validator has that validator invoked by StartDeployment, whose verdict gates the deployment — non-compliant content is refused with BadRequestException and never deploys. Smithy: appconfig CreateExtension/CreateExtensionAssociation model the action-point names, and CreateConfigurationProfile's Validators carries ValidatorType LAMBDA with the function's content Uri; both flows are synchronous request/response operations, so they must return. Recorded behavior: each flow answers within the call.
Emulator: neither flow produces an answer at all — both checks are killed at the harness cap with `Error: Check timed out after 60.0s`, with no response, no refusal and no transformed content observed. Hypothesis: the action-point dispatch awaits a Lambda invocation that never completes, so the enclosing AppConfig operation never returns; the recorded evidence is only that no answer is produced inside the budget.
Why: a caller of CreateHostedConfigurationVersion or StartDeployment on a profile with an extension or a LAMBDA validator blocks indefinitely instead of getting a response, and the validator gate — the mechanism that is supposed to stop bad configuration from reaching the fleet — can neither pass nor refuse.
Fix: the extension action-point dispatch path shared by CreateHostedConfigurationVersion (PRE_CREATE_HOSTED_CONFIGURATION_VERSION) and StartDeployment (LAMBDA Validators): the invocation must complete and its verdict/transform must be applied to the enclosing operation's response, bounded so the operation always returns. Related open issues 20261105T111951Z-appconfig-lambda-appconfig-extension-action-points-never-invoke.md, 20261107T182959Z-appconfig-lambda-the-at-deployment-tick-extension-event.md, 20261110T161251Z-appconfig-lambda-an-at-deployment-tick-handler-s.md and 20261027T064453Z-appconfig-lambda-appconfig-startdeployment-still-deploys-content-a-profile-s.md describe the same area but carry neither of these replay keys.
Traces: appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content, appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected.

## Evidence
Cause 1 — expected GetDeployment to report State from the DeploymentState enum (COMPLETE for a finished deployment); got the non-enum value `Complete`:
`AssertionError: deployment 1 never reached a terminal state within 300s; last state 'Complete' at 100.0%` (the check's terminal set is ("COMPLETE", "ROLLED_BACK", "REVERTED")). The deployment number, the 300 s poll budget and the 100.0 percentage are per-run; `Complete` vs `COMPLETE` is the divergence. Smithy: com.amazonaws.appconfig#DeploymentState = [BAKING, VALIDATING, DEPLOYING, COMPLETE, ROLLING_BACK, ROLLED_BACK, REVERTED].

Cause 2 — expected CreateHostedConfigurationVersion to return with the extension-transformed content stored, and StartDeployment to refuse validator-rejected content with BadRequestException; got no answer at all:
`Error: Check timed out after 60.0s` (appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content)
`Error: Check timed out after 60.0s` (appconfig__lambda.validator-lambda-rejects-noncompliant-content-rejected)

## Steps to Reproduce
1. git_clone(); 2. run_behavioral_suite(); 3. replay the ids in trace_ids.
Cause 1: CreateApplication/CreateEnvironment/CreateConfigurationProfile + CreateHostedConfigurationVersion, CreateDeploymentStrategy(DeploymentDurationInMinutes=2, GrowthType=LINEAR, GrowthFactor=25, FinalBakeTimeInMinutes=0), StartDeployment, then poll GetDeployment and compare State against the DeploymentState enum — observe `Complete` instead of `COMPLETE`.
Cause 2: create an extension whose PRE_CREATE_HOSTED_CONFIGURATION_VERSION action Uri is a Lambda that rewrites the content, associate it to a configuration profile, and call CreateHostedConfigurationVersion — the call never returns. Separately, create a profile whose Validators carry a LAMBDA validator that rejects the content and call StartDeployment — the call never returns instead of refusing BadRequestException.
