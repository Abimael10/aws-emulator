Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codedeploy.create-deployment-rejects-malformed-appspec-content
Issue-Shape-Id: com.amazonaws.codedeploy#CodeDeploy_20141006
Issue-Key: codedeploy.create-deployment-rejects-malformed-appspec-content|run#25|AssertionError|assert unparseable_message == _UNPARSEABLE_MESSAGE, (
Issue-Commit: 763f9e00b2baa4332096f955301d6ef3f66875d4

# codedeploy: Lambda appspec-content deployment failure emits one generic errorInformation.message instead of AWS's cause-specific text

> _Note: this issue was identified against the code base at commit `763f9e00b2baa4332096f955301d6ef3f66875d4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeployment

## Description
Cause 1 of 1: GetDeployment's errorInformation.message for an asynchronous Lambda AppSpec-content failure is re-authored as a single generic sentence, so the two distinct causes that share errorInformation.code = INVALID_LAMBDA_CONFIGURATION become indistinguishable to a caller.

AWS: CreateDeployment accepts an AppSpecContent revision whose body is syntactically broken or semantically incomplete and validates the CONTENT asynchronously, inside the deployment. The deployment reaches status Failed carrying errorInformation.code = INVALID_LAMBDA_CONFIGURATION (Smithy: GetDeployment -> DeploymentInfo.errorInformation : ErrorInformation{code: ErrorCode, message: ErrorMessage}; ErrorCode enum includes INVALID_LAMBDA_CONFIGURATION, and the shape's own doc says `message` is "An accompanying error message"). The model does not enumerate the message text, so this is recorded behavior: the one error code covers two causes, and the message is the only discriminator. AWS composes it as a fixed prefix plus a fixed cause clause — the prefix "The deployment failed because the AppSpec file that specifies the AWS Lambda deployment configuration is missing or has an invalid configuration. " followed, for a body that is not a parseable document, by "The input AppSpec file is a not well-formed yaml. The template cannot be parsed.", and for a body that parses but declares no Resources section, by "A Resources section must be specified in the Lambda AppSpec file." Every part of both messages is fixed text; no part is taken from the request or from state, so no per-run value appears in it. (Only an appSpecContent carrying neither content nor sha256 is refused synchronously, with InvalidRevisionException and message "Content and Sha256 value of the string can't both be empty" — the emulator got that case right.)

Emulator: the deployment does fail with code INVALID_LAMBDA_CONFIGURATION, but errorInformation.message is the single generic string 'The deployment failed because the AppSpec file that specifies the AWS Lambda deployment configuration is missing or has an invalid format' — it stops at "invalid format" (AWS says "invalid configuration."), omits the terminating period of the prefix, and appends no cause clause at all. The replay asserted on the unparseable-appspec deployment and failed there; because the emulator emits one constant string for the whole class, the parsed-but-no-Resources deployment cannot carry the distinct text AWS gives it either (Hypothesis: the same constant is returned for both, which the check's follow-on assertion that the two messages must differ would also catch).

Why: a client that classifies an appspec rejection by message — to tell "your YAML does not parse, fix the syntax" from "your YAML parsed but has no Resources section, add one" — gets the same opaque sentence for both, since the error code is identical in each case. There is nothing left in the response to branch on, so automated remediation and user-facing diagnostics both degrade to "something about your appspec", and a log-matching or message-asserting client simply never matches.

Fix: the Lambda appspec-content validation inside the deployment-execution path of the codedeploy service crate — where the deployment is driven to Failed and errorInformation is populated. It needs to distinguish parse failure from a parsed document missing Resources and set the cause-specific clause after the shared prefix, rather than writing one constant message for the whole class.

Traces: codedeploy.create-deployment-rejects-malformed-appspec-content (assertion at check_codedeploy_create_deployment_rejects_malformed_appspec_content.py line 134).

## Evidence
Check: codedeploy.create-deployment-rejects-malformed-appspec-content [mismatch]

Request: CreateDeployment(applicationName=<app>, deploymentGroupName=<dg>, deploymentConfigName="CodeDeployDefault.LambdaAllAtOnce", revision={revisionType: "AppSpecContent", appSpecContent: {content: 'version: 0.0\nResources: [{"broken": {"Type": \nhooks - - :\n'}}) — accepted, then polled with GetDeployment to a terminal status.

Observed (shared): status = "Failed", errorInformation.code = "INVALID_LAMBDA_CONFIGURATION" — both correct.

errorInformation.message
  expected (AWS): 'The deployment failed because the AppSpec file that specifies the AWS Lambda deployment configuration is missing or has an invalid configuration. The input AppSpec file is a not well-formed yaml. The template cannot be parsed.'
  actual (emulator): 'The deployment failed because the AppSpec file that specifies the AWS Lambda deployment configuration is missing or has an invalid format'

AssertionError: the message must say the appspec could not be PARSED, which is the only way a caller can tell this cause from the other one sharing the same code.

Message anatomy — all fixed text, nothing request- or state-derived:
  fixed prefix: "The deployment failed because the AppSpec file that specifies the AWS Lambda deployment configuration is missing or has an invalid configuration. "
  fixed cause clause, unparseable body: "The input AppSpec file is a not well-formed yaml. The template cannot be parsed."
  fixed cause clause, parsed body with no Resources section: "A Resources section must be specified in the Lambda AppSpec file."
Divergences in the emulator's text: "invalid format" instead of "invalid configuration", missing sentence-terminating period, and the entire cause clause absent.

Not in dispute / already matching: the synchronous refusal of appSpecContent={"content": ""} with InvalidRevisionException and "Content and Sha256 value of the string can't both be empty"; acceptance (non-refusal) of the malformed revision; its appearance in ListDeployments; status Failed; errorInformation.code; and the alias staying on its prior version. Only the message text diverges.

## Steps to Reproduce
1. Create a Lambda-platform CodeDeploy application and a deployment group using deploymentConfigName "CodeDeployDefault.LambdaAllAtOnce", against a versioned Lambda function with an alias pointing at v1.
2. Call CreateDeployment with revision={revisionType: "AppSpecContent", appSpecContent: {content: 'version: 0.0\nResources: [{"broken": {"Type": \nhooks - - :\n'}} — a body that is not a parseable document. It is accepted; keep the returned deploymentId.
3. Poll GetDeployment(deploymentId) until the status is terminal. Observe status == "Failed" and errorInformation.code == "INVALID_LAMBDA_CONFIGURATION".
4. Read errorInformation.message. Expected the AWS text ending "...invalid configuration. The input AppSpec file is a not well-formed yaml. The template cannot be parsed."; got "...missing or has an invalid format" with no cause clause.
5. Repeat steps 2-4 with appSpecContent.content = 'version: 0.0\n' (parses as YAML, declares no Resources section). AWS fails it with the same code but the message "...invalid configuration. A Resources section must be specified in the Lambda AppSpec file."; compare the two messages — they must differ from each other and each must match its own cause clause.
