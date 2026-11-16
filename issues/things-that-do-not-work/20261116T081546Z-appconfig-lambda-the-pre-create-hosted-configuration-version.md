Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content
Issue-Key: appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content|_deploy_and_wait_complete#59|Timeout|time.sleep(2.0)
Issue-Commit: 5636e5ddac188567344946aa5c15e82054f1c023

# appconfig__lambda: the PRE_CREATE_HOSTED_CONFIGURATION_VERSION extension action never transforms the stored content — the check never completes inside the harness cap

> _Note: this issue was identified against the code base at commit `5636e5ddac188567344946aa5c15e82054f1c023`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateHostedConfigurationVersion

## Description
One net-new root cause in this service group.

Cause 1 of 1: an AppConfig extension whose PRE_CREATE_HOSTED_CONFIGURATION_VERSION action points at a Lambda is never driven to the point where its returned replacement content is stored, so the check cannot observe either the event handed to the action or the transformed bytes and runs out of wall clock.
Failing check id: appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content.

AWS: Smithy (appconfig model): ActionPoint enum = PRE_CREATE_HOSTED_CONFIGURATION_VERSION, PRE_START_DEPLOYMENT, AT_DEPLOYMENT_TICK, ON_DEPLOYMENT_START, ON_DEPLOYMENT_STEP, ON_DEPLOYMENT_BAKING, ON_DEPLOYMENT_COMPLETE, ON_DEPLOYMENT_ROLLED_BACK; CreateExtension binds a Uri (the Lambda ARN) per action point and CreateExtensionAssociation binds an extension to a resource — here a configuration profile ARN. Beyond that, recorded behavior only: when an extension with a PRE_CREATE_HOSTED_CONFIGURATION_VERSION action is associated with a configuration profile, CreateHostedConfigurationVersion synchronously invokes that Lambda with an event carrying the submitted Content (base64), and if the action returns {"Content": "<base64>"} the RETURNED bytes are what AppConfig stores, so a subsequent GetHostedConfigurationVersion returns the transformed document; an action that returns {} leaves the submitted bytes untouched. The action invocation and the stored-bytes substitution are the requirement; the per-run stamp value, profile ids and queue URL are not.

Emulator: the check is killed by the harness before any assertion is reached — the replay detail is the whole of "Error: Check timed out after 60.0s", with no AWS error code and no leg evidence, even though the check's own budget for the transform is 600 s. Nothing in this service group's extension path completes: the sibling action-point check under the same heading is already filed as the PRE_START_DEPLOYMENT action never firing (20261106T144610Z-appconfig-lambda-the-pre-start-deployment-extension-action.md), and the remaining failures under this heading are owned by 20261115T213820Z-appconfig-lambda-2-net-new-causes-getdeployment-serves.md. Hypothesis: as with PRE_START_DEPLOYMENT, the PRE_CREATE_HOSTED_CONFIGURATION_VERSION action is never invoked, so the per-action SQS observation the check polls for never arrives and every poll runs to its deadline.

Why: a client that uses an extension to normalise, stamp or re-sign configuration before it is stored sees the raw, untransformed document served to every application that reads the profile, with no error anywhere to indicate the extension did not run. Content validation and content rewriting built on this action point are silently inert.

Fix: the AppConfig extension-invocation component on the CreateHostedConfigurationVersion path — it must resolve extension associations for the target configuration profile, invoke the Lambda bound to PRE_CREATE_HOSTED_CONFIGURATION_VERSION with the event carrying the submitted Content, and persist the action's returned Content in place of the submitted bytes when the action returns one.

Traces: appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the booted emulator: 1945/2850 passed (68.2%); 725 mismatched, 180 errored; bootstrap_signal false. Per-service appconfig__lambda 0/4 (0%), 4 failed; three of the four are marked OWNED by open issues and are excluded here.

[mismatch] appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content
  Error: Check timed out after 60.0s

That timeout is the entire diagnostic the replay carries for this check — it reached no assertion, so there is no diverging status or error code to quote. The check builds: a Lambda whose action handler forwards the event AppConfig hands it to an SQS queue (the only way to observe the event contents, since no AppConfig API reports them) and then returns {"Content": base64(document with "stamped": <stamp>)} when TRANSFORM=1; an extension with a PRE_CREATE_HOSTED_CONFIGURATION_VERSION action pointing at it; an extension association onto a configuration profile ARN.

Expected (AWS): CreateHostedConfigurationVersion on that profile invokes the action, the queue receives one message whose body is the event AppConfig sent (carrying the submitted Content), and GetHostedConfigurationVersion returns the REPLACEMENT bytes the action produced (the document with the stamp key added); with TRANSFORM unset the stored bytes equal the submitted bytes.
Actual (emulator): no observation arrives and the check is killed at the 60 s cap, with the extension never demonstrably invoked.

Smithy consulted: appconfig ActionPoint enum includes PRE_CREATE_HOSTED_CONFIGURATION_VERSION (see Description); the substitution of returned Content is recorded behavior, not modelled.

Bootstrap gate: get_bootstrap_status() reports pending == [] — appconfig and lambda are stood up in this checkout, so this is a fidelity finding, not a bootstrap gap.

## Steps to Reproduce
Boot the emulator and point appconfig, lambda, iam and sqs clients at its data listener.
1. CreateQueue; CreateFunction (python3.12) whose handler sends the whole event to that queue and then returns {"Content": base64(json.loads(base64decode(event["Content"])) + {"stamped": <stamp>})} when the env var TRANSFORM == "1", else {}.
2. CreateApplication, CreateConfigurationProfile (hosted), then CreateExtension with Actions={"PRE_CREATE_HOSTED_CONFIGURATION_VERSION": [{"Name": "transform", "Uri": <function ARN>}]} and CreateExtensionAssociation(ExtensionIdentifier=<extension>, ResourceIdentifier=<configuration profile ARN>).
3. THE GRADED CALL: CreateHostedConfigurationVersion(Application, ConfigurationProfileId, Content=json of a small document, ContentType='application/json').
   EXPECTED (AWS): the queue receives one message whose body is the event AppConfig handed the action (with the submitted Content), and GetHostedConfigurationVersion for the returned VersionNumber returns the action's replacement bytes (document with the stamp key).
   OBSERVED: no queue message and no transformed content; the check is killed at the 60 s harness cap with no assertion reached.
4. Set TRANSFORM to "0" and repeat: the stored bytes must equal the submitted bytes while the queue still receives the event.
5. Re-replay appconfig__lambda.pre-create-hosted-configuration-version-extension-transforms-stored-content.
