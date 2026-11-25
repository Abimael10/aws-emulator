Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codestar-notifications.tag-resource-reserved-prefix-key-rejected
Issue-Shape-Id: com.amazonaws.codestarnotifications#CodeStarNotifications_20191015
Issue-Key: codestar-notifications.tag-resource-reserved-prefix-key-rejected|run#12|AssertionError|assert message == expected_message, (
Issue-Commit: 551556482864271bf2a94d2347709669640f8cae

# codestar-notifications TagResource states the reserved-tag-key refusal in its own wording, not AWS's

> _Note: this issue was identified against the code base at commit `551556482864271bf2a94d2347709669640f8cae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
One net-new root cause in codestar-notifications, grounded in one failing behavioral check.

Cause 1 of 1: TagResource refuses a tag key carrying the reserved "aws:" prefix with the right shape (ValidationException, HTTP 400) but with an emulator-authored message, and it applies a wider prefix rule than AWS's. Failing check id: codestar-notifications.tag-resource-reserved-prefix-key-rejected.

  AWS: Smithy (services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/smithy-model/com-amazonaws-codestarnotifications-codestarnotifications-20191015.json): TagResource, http POST /tagResource code 200, input TagResourceRequest{Arn -> NotificationRuleArn @required @pattern ^arn:aws[^:\s]*:codestar-notifications:[^:\s]+:\d{12}:notificationrule\/(.*\S)?$, Tags -> Tags @required, documented as "The list of tags to associate with the resource. Tag key names cannot start with \"aws\"."}; modeled errors ConcurrentModificationException (400), LimitExceededException (400), ResourceNotFoundException (404), ValidationException (400, "One or more parameter values are not valid."). The model states the CONDITION only; the wire text is recorded behavior. The recording shows AWS refusing a reserved key with HTTP 400, error code ValidationException and the message template:
    "One or more of the following tag keys is reserved: [<offending keys>]. Tag keys cannot begin with the characters \"aws:\". For more information, see the AWS CodeStar Notifications API Reference."
  Fixed text: everything outside the bracketed list — "One or more of the following tag keys is reserved: [", then "]. Tag keys cannot begin with the characters \"aws:\". For more information, see the AWS CodeStar Notifications API Reference." Variable part: ONLY the bracketed list, which is the submitted tag key(s) from the request that begin with the reserved prefix (in the replayed example the single key aws:reserved — that value fills the template, it is not part of it). The message itself states the rule AWS applies: the reserved prefix is the four characters "aws:", not the three characters "aws".

  Emulator: answers HTTP 400 with code ValidationException (correct) but message 'Tags key names cannot start with "aws": aws:reserved' — a different sentence, different word order, no bracketed key list, and the offending key appended after a colon instead of interpolated into a list. The text is minted by crate::state::rule_tags_from and lifted into the modeled shape by tag_resource's invalid() helper. The same helper also implements the rule as a case-insensitive "aws" PREFIX test (key.to_ascii_lowercase().starts_with("aws")), so it additionally refuses keys AWS accepts — any key merely beginning with the letters aws without the colon (e.g. "awsome", "AWSteam") is rejected, where AWS reserves only keys beginning with "aws:".

  Why: a client that classifies or surfaces tag-validation failures by the service's message text (log scraping, user-facing error surfacing, retry/branching on the reserved-prefix class, or test assertions against the documented API-reference sentence) does not recognise the emulator's refusal, even though the status and error code match. The wider prefix test is worse than a wording difference: a caller tagging a rule with a legitimate key such as "awsome-team" is refused against the emulator and accepted by AWS, so the tag never lands and the client sees a spurious ValidationException.

  Fix: the shared tag-key validation and its message in crate::state (the rule_tags_from helper that both TagResource and the CreateNotificationRule tagging path call), not the TagResource handler flow, which already produces the correct shape and status: the refusal must word AWS's sentence with the offending submitted keys as the bracketed list, and the reserved prefix must be "aws:" rather than "aws". File/line citations are under Traces/Evidence.

  Traces: codestar-notifications.tag-resource-reserved-prefix-key-rejected.

Note: the three open codestar-notifications issues (rule-identifier minting and the two ListNotificationRules NotificationRuleSummary.Id issues, 20261110T174500Z-..., 20261110T182312Z-..., 20261110T210013Z-...) concern rule id provenance on 200 responses and do NOT cover this tagging-refusal message.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 22/23 passed (95.7%), 1 mismatched, 0 errored, 0 skipped, 0 unloadable. Only service exercised: codestar-notifications 22/23 (96%), 1 failed. bootstrap_signal false (the emulator serves — it answered 22 checks and returned a modeled ValidationException for this one).

Failing check: [mismatch] codestar-notifications.tag-resource-reserved-prefix-key-rejected
  Assertion detail (from the replay output):
    AssertionError: reserved-tag-key refusal message: expected 'One or more of the following tag keys is reserved: [aws:reserved]. Tag keys cannot begin with the characters "aws:". For more information, see the AWS CodeStar Notifications API Reference.', got 'Tags key names cannot start with "aws": aws:reserved'

What matched and what did not:
  - HTTP status: 400 both sides (the check reached the message assertion only after code == 'ValidationException' passed).
  - Error code: ValidationException both sides — matches.
  - Error message: diverges, as quoted above.
  - Effect: the check's surrounding assertions passed — the rule stays untagged after the refusal, and the positive control (TagResource with key 'env') echoes {'env': ...} and is visible through ListTagsForResource. So only the refusal TEXT (and the prefix rule behind it) is at fault.

Expected message template (fixed text + slot):
  "One or more of the following tag keys is reserved: [" + <comma-separated submitted keys beginning with "aws:"> + "]. Tag keys cannot begin with the characters \"aws:\". For more information, see the AWS CodeStar Notifications API Reference."
  In the replayed call the slot held the one submitted key, aws:reserved. That key came from the REQUEST; it is a fill-in, not a requirement.

Actual message template, as the emulator builds it:
  "Tags key names cannot start with \"aws\": " + <first submitted key whose lowercase form starts with "aws">.

Code under the cause (same checkout the emulator is built from):
  services/service-com-amazonaws-codestarnotifications-codestarnotifications-20191015/src/state.rs:758-769 — rule_tags_from(): `if key.to_ascii_lowercase().starts_with("aws") { return Err(format!("Tags key names cannot start with \"aws\": {key}")); }` — both the wording and the three-character prefix rule live here, and the loop returns on the FIRST offending key rather than collecting all of them (AWS's message lists keys).
  .../src/ops/tag_resource.rs:190-192 — `let supplied = st::rule_tags_from(Some(&input.tags)).map_err(invalid)?;` — the handler lifts that string straight into the modeled ValidationException.
  .../src/ops/tag_resource.rs:110-117 — invalid(): ValidationException { message: Some(...) }.
  The same helper is documented at state.rs:754-757 as the one the CreateNotificationRule tagging path uses, so the create path carries the identical wording and prefix rule.

Smithy consulted: TagResource errors include ValidationException @httpError 400; TagResourceRequest.Tags is @required with the documentation sentence quoted above. The model fixes the status and the error shape (both correct here) and says nothing further about the message text, which is why the recorded refusal text is the requirement.

## Steps to Reproduce
1. Clone the repo and let the behavioral suite build and boot the emulator from this checkout.
2. Replay check codestar-notifications.tag-resource-reserved-prefix-key-rejected (suite simparity-behavioral-aws-v1).
3. The check creates a CodeDeploy application and an SNS topic with a codestar-notifications publish policy, then CreateNotificationRule (DetailType BASIC, EventTypeIds [codedeploy-application-deployment-succeeded]) and asserts the fresh rule is untagged — all pass.
4. It then calls TagResource(Arn=<rule arn>, Tags={"aws:reserved": "nope"}). The emulator answers HTTP 400 ValidationException (correct shape) with message 'Tags key names cannot start with "aws": aws:reserved'.
5. The check asserts the message equals AWS's: 'One or more of the following tag keys is reserved: [aws:reserved]. Tag keys cannot begin with the characters "aws:". For more information, see the AWS CodeStar Notifications API Reference.' -> AssertionError, verdict mismatch.
6. Reproduce the second half of the cause directly: call TagResource(Arn=<rule arn>, Tags={"awsome": "ok"}). The emulator refuses it with the same ValidationException (its prefix test is the three letters "aws", case-insensitive); AWS reserves only keys beginning with "aws:" and accepts this key.
7. After a fix, re-replay codestar-notifications.tag-resource-reserved-prefix-key-rejected: it must pass, i.e. the refusal keeps HTTP 400 / ValidationException and words AWS's sentence with the submitted reserved key(s) as the bracketed list, while a non-reserved key such as "awsome" is accepted and readable back through ListTagsForResource.
