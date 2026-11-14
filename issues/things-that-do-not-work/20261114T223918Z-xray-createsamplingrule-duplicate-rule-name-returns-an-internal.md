Issue-Source: trace-sweep-divergence
Issue-Trace: xray-sampling-rule-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: xray-sampling-rule-lifecycle
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray|CreateSamplingRule|4xx->4xx|$.Message
Issue-Commit: 9bc67a45b2009c5bd448142e42eeb203770a9088

# xray CreateSamplingRule duplicate rule name returns an internal resource-collision message instead of "Sampling rule already exists"

> _Note: this issue was identified against the code base at commit `9bc67a45b2009c5bd448142e42eeb203770a9088`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSamplingRule

## Description
One cause.

Cause 1 of 1: CreateSamplingRule rejects a duplicate sampling-rule name with the wrong error message (fixture xray-sampling-rule-lifecycle, step task#2).

AWS: Smithy (services/service-com-amazonaws-xray-awsxray/smithy-model) models CreateSamplingRule with input SamplingRule (required) and errors InvalidRequestException (client, httpError 400), RuleLimitExceededException (400) and ThrottledException (429). The model does not spell out the duplicate-name message, so for the message itself this is recorded behavior only: the recording shows that re-creating a sampling rule whose RuleName already exists answers HTTP 400 with x-amzn-errortype InvalidRequestException and the body message field `Message` set to exactly `Sampling rule already exists`. That string is entirely FIXED text — it interpolates nothing from the request or from service state: no rule name, no ARN, no identifier.

Emulator: the emulator answers the same HTTP 400 and the same error type, but with `Message` = `A resource with the same resourceName but a different internalId already exists: 729a219d5945f279` — an internal resource-registry collision message whose trailing hex token is a per-run generated internal id. So the fixed text is wrong AND the emulator has injected a volatile internal value into a message that AWS defines as constant.

Why: a client relying on AWS's behavior gets the wrong signal for an idempotent/retry path. Callers that create-or-reuse a sampling rule branch on the duplicate-name message (the error code InvalidRequestException is shared by every malformed-request rejection on this operation, so the message is the only discriminator), and that branch never fires here. Worse, the emulator's message is not stable across runs because it embeds a generated internal id, so no client or test can match it at all, and the leaked internal id exposes emulator implementation detail that no AWS response contains.

Fix: the CreateSamplingRule handler's duplicate-name rejection path (its validation/conflict check) must raise InvalidRequestException with the constant message `Sampling rule already exists` rather than falling through to the shared generic resource-registry collision error used by the emulator's state layer. The generic registry error should not be the response surface for this operation; the handler must detect the name conflict itself and construct the modeled error.

Traces: xray-sampling-rule-lifecycle#task#2 (expected 400 / `Sampling rule already exists`, got 400 / `A resource with the same resourceName but a different internalId already exists: <internal-id>`).

## Evidence
Fixture xray-sampling-rule-lifecycle, step task#2, operation CreateSamplingRule.
Expected status: 400. Actual status: 400 (status matches).
Divergence detail from the replay: `$.Message: value_mismatch (expected 'Sampling rule already exists', got 'A resource with the same resourceName but a different internalId already exists: 729a219d5945f279')`.
Fixed text required by AWS: the whole message, `Sampling rule already exists`. Variable parts: none.
Variable parts the emulator wrongly emits: the trailing `729a219d5945f279` is a per-run generated internal id and is not part of any AWS message template; it is quoted here only as the recorded instance.
Error type header matched (InvalidRequestException); only the message body diverges.
Smithy: CreateSamplingRule errors = InvalidRequestException (400, client), RuleLimitExceededException (400), ThrottledException (429); http POST /CreateSamplingRule, success code 200.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus: replay_wire_corpus().
3. Fixture xray-sampling-rule-lifecycle creates a sampling rule (task#1), then calls CreateSamplingRule again at task#2 with the SAME SamplingRule.RuleName.
4. Observe step task#2: the emulator returns HTTP 400 with InvalidRequestException but body $.Message = 'A resource with the same resourceName but a different internalId already exists: <hex>' instead of the recorded AWS message 'Sampling rule already exists'.
