Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns.check-phone-number-opted-out-invalid-fails sns.op-confirm-subscription-invalid-token-rejected
Issue-Shape-Id: com.amazonaws.sns#AmazonSimpleNotificationService
Issue-Key: sns.check-phone-number-opted-out-invalid-fails|_error#6|AssertionError|raise AssertionError(f"expected rejection, but the call SUCCEEDED and returned {result!r}")
Issue-Key: sns.op-confirm-subscription-invalid-token-rejected|run#6|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: 11009514ecfa49e7fbff448c76359031cc8c2c5e

# sns: 2 net-new causes — CheckIfPhoneNumberIsOptedOut accepts a value that is not a phone number at all (200 isOptedOut=false instead of 400 InvalidParameter), and ConfirmSubscription answers 'Invalid token' for a token of invalid FORM where AWS says 'Invalid parameter: Token'

> _Note: this issue was identified against the code base at commit `11009514ecfa49e7fbff448c76359031cc8c2c5e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CheckIfPhoneNumberIsOptedOut

## Description
TWO net-new root causes of service `sns`, grounded in the two failing behavioral checks under the `sns` heading that are neither ALREADY FILED nor OWNED by an open issue. BOTH must be closed before this issue is submitted.

Fixer context (read first): the OPEN wire issue 20261116T130313Z-sns-5-wire-divergences-the-sms-opt-out.md describes the OPPOSITE side of each of these two boundaries — its Cause 1 is CheckIfPhoneNumberIsOptedOut refusing 400 a plausible number AWS answers 200 for, and its Cause 3 is ConfirmSubscription re-wording 'Invalid token' as 'Invalid parameter: Token'. It does not cover these two checks (they are not among its replay keys), and the emulator has since swung to the other extreme on both. Each operation has TWO conditions with TWO different answers; a fix that satisfies only one side re-breaks the other, so both sides must hold simultaneously.

=== Cause 1 of 2: CheckIfPhoneNumberIsOptedOut accepts a value that is not a phone number ===
Failing check: sns.check-phone-number-opted-out-invalid-fails.
AWS: Smithy (com.amazonaws.sns#CheckIfPhoneNumberIsOptedOut) — input member `phoneNumber` is @required and @sensitive; output member `isOptedOut` is a boolean defaulting to false; the operation's client refusal for a bad member is InvalidParameterException (errorKind client, httpError 400, awsQueryError code `InvalidParameter`). Recorded behavior: a plausible number with no stored opt-out record is answered HTTP 200 with isOptedOut=false, but a value that is not a phone number at all is refused HTTP 400 / `InvalidParameter` with the message 'Invalid parameter: PhoneNumber Reason: input incorrectly formatted'. That message is AWS's fixed `Invalid parameter: <member> Reason: <reason>` frame for this operation; its only variable part is the member name 'PhoneNumber' — nothing is taken from the offending value, and the submitted string is not echoed.
Emulator: the malformed value is ACCEPTED. The call answers HTTP 200 with `{'isOptedOut': False}` and no Error at all, so the refusal class disappears entirely. Regression context: the resolved issue 20261113T225447Z-sns-5-net-new-causes-the-notification-envelope.md carried the cause 'CheckIfPhoneNumberIsOptedOut accepts a non-number' and closed it; it is broken again, and the open wire issue above asked for the over-narrow E.164 rule to be loosened, which appears to have removed the formatting judgement rather than narrowing it.
Why: this read is the pre-flight a sender runs before an SMS send. A client that passes a corrupted or unparsed value (a name, a URL, an empty-ish token) is told by AWS that its request is malformed and fixes it at the call site; against the emulator it is told isOptedOut=false, i.e. 'go ahead and message this', so a client-side formatting bug is laundered into a 200 and surfaces only later, and any negative test on the InvalidParameter class passes against AWS and fails here.
Fix: the CheckIfPhoneNumberIsOptedOut handler's argument validation (the operation's handler and the shared phone-number refusal rule it uses) — restore the formatting verdict for a value that is not a phone number, refusing it 400 `InvalidParameter` with exactly 'Invalid parameter: PhoneNumber Reason: input incorrectly formatted', while still answering 200 isOptedOut=false for any plausible number that holds no opt-out record (the boundary the open wire issue's Cause 1 requires). The rule must not be the constrained PhoneNumberString pattern of the sandbox/publish members, and it must not be nothing.
Traces: sns.check-phone-number-opted-out-invalid-fails.

=== Cause 2 of 2: ConfirmSubscription uses the wrong one of its two refusal sentences for a token of invalid FORM ===
Failing check: sns.op-confirm-subscription-invalid-token-rejected.
AWS: Smithy (com.amazonaws.sns#ConfirmSubscription) takes @required TopicArn and @required Token and declares InvalidParameterException (client, httpError 400, awsQueryError `InvalidParameter`) and NotFoundException (404). Recorded behavior: SNS distinguishes two conditions with two different fixed messages under the same 400/`InvalidParameter` pair — a Token whose FORM is not a confirmation token is refused with 'Invalid parameter: Token' (the standard `Invalid parameter: <member>` frame, variable part = the member name 'Token'), while a well-formed token that matches no pending confirmation of the named topic is refused with 'Invalid token' (wholly fixed text). Neither refusal creates a subscription, so ListSubscriptionsByTopic stays empty.
Emulator: status 400 and code `InvalidParameter` are correct, but the message for a token of invalid form (the check sends a 64-character run of one letter) is 'Invalid token' — the emulator now serves the match-nothing sentence for both conditions, collapsing the two classes into one. The check asserts the message and fails: expected 'Invalid parameter: Token', got 'Invalid token'.
Why: the message is the only machine-readable carrier of the distinction between 'your request member is malformed' (a client bug, never retryable) and 'this confirmation link confirms nothing' (an expired or already-used token, the user must request a new one). A confirmation endpoint, matcher or log pipeline keyed on AWS's text misclassifies every malformed-token refusal as an expired link and can loop re-issuing confirmations.
Fix: the ConfirmSubscription handler's token validation and the state constant it returns — judge the token's FORM first and refuse that class with 'Invalid parameter: Token', and keep 'Invalid token' for a well-formed token matching no pending confirmation of the named topic. Two conditions, two sentences, both 400 `InvalidParameter`, neither creating a subscription.
Traces: sns.op-confirm-subscription-invalid-token-rejected.

## Evidence
Behavioral suite replay against the booted emulator (521/602 passed, bootstrap_signal=false). Service heading `sns`: 235/245, 10 failed — 8 of them OWNED by open issues and skipped; the two below carry no ALREADY FILED and no OWNED marker.

Cause 1 — [mismatch] sns.check-phone-number-opted-out-invalid-fails
  Error: File "check_sns_check_phone_number_opted_out_invalid_fails.py", line 38, in run / line 24, in _error
  AssertionError: expected rejection, but the call SUCCEEDED and returned {'isOptedOut': False, 'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'text/xml', ...}}}
  The check first proves the positive control (a plausible +1500555NNNN number returns isOptedOut), then calls check_if_phone_number_is_opted_out(phoneNumber='not-a-phone') and requires code == 'InvalidParameter' and message == 'Invalid parameter: PhoneNumber Reason: input incorrectly formatted'.
  Expected (AWS): HTTP 400, Error.Code 'InvalidParameter', Error.Message 'Invalid parameter: PhoneNumber Reason: input incorrectly formatted' (fixed frame; variable part is the member name 'PhoneNumber' only).
  Actual (emulator): HTTP 200 with isOptedOut=false and no Error member.

Cause 2 — [mismatch] sns.op-confirm-subscription-invalid-token-rejected
  Error: File "check_sns_op_confirm_subscription_invalid_token_rejected.py", line 47, in run
  AssertionError: expected message 'Invalid parameter: Token', got 'Invalid token'
  The check creates a topic and calls confirm_subscription(TopicArn=<topic>, Token='a'*64); it asserts code == 'InvalidParameter' (which the emulator gets right), then the message, then that ListSubscriptionsByTopic is still empty.
  Expected (AWS): HTTP 400, Error.Code 'InvalidParameter', Error.Message 'Invalid parameter: Token' for a token of invalid form (and 'Invalid token' reserved for a well-formed token that matches no pending confirmation).
  Actual (emulator): HTTP 400, Error.Code 'InvalidParameter', Error.Message 'Invalid token' — the match-nothing sentence served for the malformed-form condition.

Neither finding depends on a per-run value: the RequestId, the generated topic name and the control phone number in the recordings are per-run and are not part of either requirement.

## Steps to Reproduce
0. git_clone the repo, build and boot the emulator, then re-replay exactly the two checks in trace_ids.
1. Cause 1: CheckIfPhoneNumberIsOptedOut(phoneNumber='+1500555XXXX') — must answer HTTP 200 with isOptedOut=false (control, must keep passing). Then CheckIfPhoneNumberIsOptedOut(phoneNumber='not-a-phone') and read the status, Error.Code and Error.Message — AWS refuses HTTP 400 / 'InvalidParameter' / 'Invalid parameter: PhoneNumber Reason: input incorrectly formatted'; the emulator answers HTTP 200 with isOptedOut=false. Both boundaries must hold at once.
2. Cause 2: CreateTopic, then ConfirmSubscription(TopicArn=<topic>, Token='a'*64) and read Error.Code and Error.Message — AWS states 'Invalid parameter: Token'; the emulator states 'Invalid token'. Then ConfirmSubscription(TopicArn=<topic>, Token=<a well-formed confirmation token that confirms nothing>) — that one must state 'Invalid token'. After both, ListSubscriptionsByTopic(TopicArn=<topic>) must return an empty Subscriptions list.
3. Re-check against the open wire issue 20261116T130313Z-sns-5-wire-divergences-the-sms-opt-out.md (its Causes 1 and 3): the wire fixtures assert the other side of each boundary, so run both corpora after the fix.
