Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.tag-resource-exceeds-tag-limit-rejected bedrock.tag-resource-quota-exceeded-rejected
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock.tag-resource-exceeds-tag-limit-rejected|run#6|AssertionError|assert message == "You can assign a maximum of 50 tags to a resource", f"unexpected rejection message: {message!r}"
Issue-Key: bedrock.tag-resource-quota-exceeded-rejected|run#13|AssertionError|assert message == "You can assign a maximum of 50 tags to a resource", (
Issue-Commit: 8ebbdfd1f1f0cf009ce2275a580b7bd57695be30

# bedrock TagResource's TooManyTagsException message is emulator-authored ("The resultant tag set must not have more than 50 user tags.") instead of AWS's "You can assign a maximum of 50 tags to a resource"

> _Note: this issue was identified against the code base at commit `8ebbdfd1f1f0cf009ce2275a580b7bd57695be30`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
One net-new root cause in the bedrock control plane's tagging surface.

Cause 1 of 1: TagResource correctly detects the per-resource tag quota and correctly classifies the refusal as TooManyTagsException with HTTP 400, but it serves its own sentence for the error message rather than the sentence Amazon Bedrock emits.

AWS: Smithy (com.amazonaws.bedrock) models TagResource with error TooManyTagsException, errorKind client, httpError 400; the shape's own documentation states the rule — "The request contains more tags than can be associated with a resource (50 tags per resource). The maximum number of tags includes both existing tags and those included in your current request." The request member tags targets TagList with @length min 0 / max 200, so a request of 60 or 197 tag entries is shape-valid and the 50-per-resource business quota is what refuses it — not a plain ValidationException. The recorded Bedrock response for that refusal carries HTTP 400, Error.Code "TooManyTagsException" and Error.Message exactly "You can assign a maximum of 50 tags to a resource". That message is entirely FIXED text: it has no variable parts interpolated from the request or from state — the limit 50 is the constant service quota, and neither the offending resource ARN, the submitted tag count, nor any tag key appears in it. (The TooManyTagsException shape additionally carries an optional resourceName member for naming the resource; the message itself stays constant.)

Emulator: the refusal arrives with the right status (400) and the right code (TooManyTagsException), but Error.Message is "The resultant tag set must not have more than 50 user tags." — a differently-worded sentence (it reads like the IAM-family tag-limit wording), not Bedrock's. Observed identically for both shapes of the quota breach: a single 60-tag call against a guardrail ARN, and a 197-tag call that would push an automated-reasoning-policy from 5 existing tags to 202.

Why: a client that branches on, logs, or asserts the refusal text — error-message matching is common in tagging/quota retry and remediation code, and in test suites pinned to AWS's documented sentence — sees a message real Bedrock never emits. Automation keyed to "You can assign a maximum of 50 tags to a resource" falls through to a generic/unknown-error path against the emulator while succeeding against AWS, and operators reading the emulator's text are told the resultant-set rule in wording Bedrock does not use. The quota itself, the status and the code are all correct, so only the message diverges.

Fix: the message constant used when the bedrock control-plane TagResource handler's tag-quota validation raises TooManyTagsException. It must emit the fixed sentence "You can assign a maximum of 50 tags to a resource" with no interpolation; the surrounding quota arithmetic (existing tags + requested tags > 50) and the 400/TooManyTagsException classification already match AWS and should not change. The same constant should be used wherever the bedrock crate raises TooManyTagsException (the quota applies to every taggable Bedrock resource type, and both failing checks reach it through different resource types — guardrail and automated-reasoning-policy — so this is one shared message, not two).

Traces: bedrock.tag-resource-exceeds-tag-limit-rejected (60 tags on a guardrail ARN; asserted at check_bedrock_tag_resource_exceeds_tag_limit_rejected.py line 35) and bedrock.tag-resource-quota-exceeded-rejected (5 existing + 197 new tags on an automated-reasoning-policy ARN; asserted at check_bedrock_tag_resource_quota_exceeded_rejected.py line 34). In both checks every other assertion on the refusal passes — the code is TooManyTagsException, no tags from the rejected batch are persisted, and the within-quota positive/restore calls are accepted and listed — so the message is the only divergence.

No open issue covers this: the existing bedrock control-plane issue 20261029T172312Z-bedrock-control-plane-is-over-permissive-enforced-guardrail.md is about calls that are wrongly ACCEPTED, and 20261026T233704Z-bedrock-batchdeleteevaluationjob-s-non-terminal-status-refusal-uses.md is about BatchDeleteEvaluationJob's status-conflict wording, not the tagging quota.

## Evidence
Expected (real AWS / recorded): HTTP 400, Error.Code = "TooManyTagsException", Error.Message = "You can assign a maximum of 50 tags to a resource" (fixed text, no request- or state-derived values).

Actual (emulator): HTTP 400, Error.Code = "TooManyTagsException" (correct), Error.Message = "The resultant tag set must not have more than 50 user tags."

Assertion output, check bedrock.tag-resource-exceeds-tag-limit-rejected:
  AssertionError: unexpected rejection message: 'The resultant tag set must not have more than 50 user tags.'
  (assert message == "You can assign a maximum of 50 tags to a resource")

Assertion output, check bedrock.tag-resource-quota-exceeded-rejected:
  AssertionError: unexpected TooManyTagsException message 'The resultant tag set must not have more than 50 user tags.'
  (assert message == "You can assign a maximum of 50 tags to a resource")

Smithy evidence used: TagResource -> errors include TooManyTagsException (client, httpError 400); input member tags -> TagList, @length {min: 0, max: 200}; TooManyTagsException doc: "The request contains more tags than can be associated with a resource (50 tags per resource). The maximum number of tags includes both existing tags and those included in your current request."

## Steps to Reproduce
A) Guardrail, single over-limit batch:
1. bedrock:CreateGuardrail with any valid name, blockedInputMessaging/blockedOutputsMessaging and one contentPolicyConfig filter; poll GetGuardrail until status READY.
2. Call bedrock:TagResource with resourceARN = the guardrail ARN and tags = 60 entries [{key: "k<i>", value: "v<i>"}] (over the 50-per-resource quota, under the shape's 200-item cap).
3. Observe the refusal: HTTP 400 / TooManyTagsException is correct, but Error.Message is "The resultant tag set must not have more than 50 user tags." instead of "You can assign a maximum of 50 tags to a resource".
4. bedrock:ListTagsForResource on the same ARN returns [] (correct — nothing was persisted), and a subsequent single-tag TagResource is accepted (correct).

B) Automated-reasoning policy, existing + new crossing the quota:
1. bedrock:CreateAutomatedReasoningPolicy with any valid name; take policyArn.
2. TagResource with 5 tags (accepted; ListTagsForResource shows 5).
3. TagResource with 197 further tags, so existing + requested = 202 > 50.
4. Observe the same wrong Error.Message on the TooManyTagsException refusal; the prior 5 tags correctly survive and a later 3-tag call correctly brings the total to 8.

Re-replay both behavioral checks to confirm: bedrock.tag-resource-exceeds-tag-limit-rejected and bedrock.tag-resource-quota-exceeded-rejected.
