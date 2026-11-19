Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__s3.put-event-selectors-neither-or-both-selectors-rejected
Issue-Key: cloudtrail|PutEventSelectors|InvalidEventSelectorsException
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# PutEventSelectors rejection with neither selector kind returns a non-AWS error message

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEventSelectors

## Description
Cause 1 of 1: CloudTrail PutEventSelectors refuses a request that names neither EventSelectors nor AdvancedEventSelectors, but the emulator serves its own paraphrased message instead of the message AWS words for that refusal.

AWS: Smithy (services/service-com-amazonaws-cloudtrail-cloudtrail-20131101) models PutEventSelectorsRequest with only TrailName @required; EventSelectors and AdvancedEventSelectors are both optional lists, and the selector-kind exclusivity rule is enforced at runtime via InvalidEventSelectorsException (errorKind client, httpError 400). The model does not state the message text, so recorded behavior only for the wording: AWS answers HTTP 400 with Code "InvalidEventSelectorsException" and Message "You must select events by using either EventSelectors or AdvancedEventSelectors, but you cannot use both." That message is entirely fixed text for this error class — it contains no value taken from the request or from state (no trail name, ARN, member name or count), so it must be reproduced verbatim. The sibling refusal, a call naming BOTH selector kinds, is the same code with its own fixed text: "You can select events by using either EventSelectors or AdvancedEventSelectors, but not both."

Emulator: for PutEventSelectors(TrailName=<trail>) with neither selector list present, the emulator returns Code "InvalidEventSelectorsException" (correct) but Message "A PutEventSelectors request must carry either EventSelectors or AdvancedEventSelectors" — different fixed text, and it also does not mention the "but you cannot use both" clause AWS includes in this same message. The check asserted the code first and passed, then failed on the message, so the status/code pair is right and only the message text diverges. The both-selectors branch was never reached in this replay (the check aborts at the neither-selectors assertion), so its wording is unverified and should be checked against the AWS text quoted above while fixing this.

Why: clients and tooling match CloudTrail refusals on the message, not only the code, because InvalidEventSelectorsException covers many distinct validation failures (neither kind, both kinds, bad field selector, unsupported data resource type) under one code. A caller that branches on the AWS text — or a test/snapshot that asserts it — takes the wrong branch or fails against the emulator even though the call was refused for the right reason, and operators reading the emulator's message get guidance that omits the mutual-exclusion half of the rule.

Fix: the PutEventSelectors handler's request validation in the CloudTrail service crate — the branch that detects an absent/conflicting selector kind and constructs InvalidEventSelectorsException. Keep the 400 and the code; replace the hand-written message with AWS's fixed text for each of the two branches.

Traces: cloudtrail__s3.put-event-selectors-neither-or-both-selectors-rejected.

## Evidence
Failing check cloudtrail__s3.put-event-selectors-neither-or-both-selectors-rejected, at the call ct.put_event_selectors(TrailName=name) with no selector list:

Expected (AWS):
  HTTP 400
  Error.Code    = "InvalidEventSelectorsException"
  Error.Message = "You must select events by using either EventSelectors or AdvancedEventSelectors, but you cannot use both."

Actual (emulator):
  Error.Code    = "InvalidEventSelectorsException"   (matches)
  Error.Message = "A PutEventSelectors request must carry either EventSelectors or AdvancedEventSelectors"

Replay output:
  File "check_cloudtrail__s3_put_event_selectors_neither_or_both_selectors_rejected.py", line 157, in run
    expect_error(lambda: ct.put_event_selectors(TrailName=name), *_NEITHER)
  File "check_cloudtrail__s3_put_event_selectors_neither_or_both_selectors_rejected.py", line 34, in expect_error
    assert err.get("Message") == message, err
  AssertionError: {'Message': 'A PutEventSelectors request must carry either EventSelectors or AdvancedEventSelectors', 'Code': 'InvalidEventSelectorsException'}

Message anatomy: both AWS messages are 100% fixed text for this error class; no part is interpolated from the request or state. The emulator's text is a paraphrase, not a different interpolation.

Not verified in this replay (same handler, fix together): the both-selectors branch, where AWS answers Code "InvalidEventSelectorsException", Message "You can select events by using either EventSelectors or AdvancedEventSelectors, but not both."

## Steps to Reproduce
1. Create an S3 bucket with the standard CloudTrail bucket policy (s3:GetBucketAcl + s3:PutObject for cloudtrail.amazonaws.com).
2. ct.create_trail(Name=<trail>, S3BucketName=<bucket>).
3. Positive control: ct.put_event_selectors(TrailName=<trail>, EventSelectors=[{"ReadWriteType": "ReadOnly", "IncludeManagementEvents": True}]) — accepted, and get_event_selectors echoes it.
4. Call ct.put_event_selectors(TrailName=<trail>) with NEITHER EventSelectors nor AdvancedEventSelectors.
   Observe: ClientError with Code "InvalidEventSelectorsException" but Message "A PutEventSelectors request must carry either EventSelectors or AdvancedEventSelectors" instead of AWS's "You must select events by using either EventSelectors or AdvancedEventSelectors, but you cannot use both."
5. Then call ct.put_event_selectors(TrailName=<trail>, EventSelectors=[{"ReadWriteType": "All", "IncludeManagementEvents": True}], AdvancedEventSelectors=[{"Name": "mgmt-only", "FieldSelectors": [{"Field": "eventCategory", "Equals": ["Management"]}]}]) and compare the message against AWS's "You can select events by using either EventSelectors or AdvancedEventSelectors, but not both."
6. In both cases get_event_selectors(TrailName=<trail>) must still return the ReadOnly selector from step 3 unchanged.
