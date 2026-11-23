Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.tag-instance-profile-duplicate-key-rejected iam.tag-policy-duplicate-key-rejected iam.tag-aws-managed-policy-rejected iam.tag-user-fifty-tag-cap-overwrite-does-not-consume-it iam.update-signing-certificate-invalid-status-value-rejected iam.op-contract-resync-mfa-device
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.tag-instance-profile-duplicate-key-rejected|_error#8|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: iam.tag-policy-duplicate-key-rejected|_error#6|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: iam.tag-aws-managed-policy-rejected|run#5|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: iam.tag-user-fifty-tag-cap-overwrite-does-not-consume-it|run#19|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: iam.update-signing-certificate-invalid-status-value-rejected|run#13|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: iam.op-contract-resync-mfa-device|_wait_past_totp_window#15|Timeout|time.sleep(1)
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# iam: 5 net-new causes — duplicate tag keys accepted, three tagging/validation messages diverge, ResyncMFADevice never answers

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagPolicy

## Description
Five further net-new root causes in IAM, all in tagging and input-validation paths.

Cause 1 of 5: a Tags list containing the same key twice is accepted.
AWS: Smithy (iam TagPolicy, TagInstanceProfile — `Tags: tagListType`, required, max 50) declares InvalidInputException (400, awsQueryError InvalidInput); AWS refuses a tag list with a duplicate key and stores nothing.
Emulator: both calls return 200 with an empty body (`expected the call to be rejected, but it succeeded: {'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}`), so one of the two values silently wins.
Why: the client's request is ambiguous and AWS says so; here the emulator picks a value, so tag state depends on an ordering AWS never applies, and the client never learns its request was malformed.
Fix: the shared IAM tag-input validation used by the Tag* operations — reject a duplicate key with InvalidInputException before any state write.
Traces: iam.tag-instance-profile-duplicate-key-rejected, iam.tag-policy-duplicate-key-rejected.

Cause 2 of 5: TagPolicy's refusal for an AWS managed policy is not AWS's message.
AWS: recorded behavior only for the text — the refusal message is the fixed sentence `Tags not supported for policies in this domain` (no request-derived parts).
Emulator: `Policy arn:aws:iam::aws:policy/ReadOnlyAccess is an AWS managed policy, which cannot be tagged.` — a bespoke sentence that embeds the policy ARN.
Why: a client matching the documented message does not recognise the refusal.
Fix: the TagPolicy handler's AWS-managed-policy refusal message.
Traces: iam.tag-aws-managed-policy-rejected.

Cause 3 of 5: the 50-tag cap refusal message is not AWS's.
AWS: Smithy TagUser caps `Tags` at 50; the recorded over-cap message is the fixed sentence `The number of tags has reached the maximum limit.` (no request-derived parts). Re-tagging an existing key does not consume cap.
Emulator: `Cannot assign more than 50 tags to IAM user <user>` — bespoke text naming the user.
Why: same message-contract break; clients branching on the limit message mis-handle it.
Fix: the tag-cap enforcement's message in the IAM tagging path.
Traces: iam.tag-user-fifty-tag-cap-overwrite-does-not-consume-it.

Cause 4 of 5: UpdateSigningCertificate's enum validation message lists the allowed values in the wrong order.
AWS: Smithy UpdateSigningCertificate.Status targets `statusType`; the recorded validation message is `1 validation error detected: Value at 'status' failed to satisfy constraint: Member must satisfy enum value set: [Expired, Active, Inactive]` — fixed text, with the value set in that order.
Emulator: `... Member must satisfy enum value set: [Active, Inactive, Expired]`.
Why: the message is a requirement, and a client asserting on (or displaying) the documented text sees a different set ordering for the same error.
Fix: the enum value-set ordering used when serializing this validation message.
Traces: iam.update-signing-certificate-invalid-status-value-rejected.

Cause 5 of 5: the ResyncMFADevice contract check never completes.
AWS: Smithy ResyncMFADevice declares InvalidAuthenticationCodeException/NoSuchEntityException and answers immediately; the recording shows the bad-code refusal `Authentication code for device is not valid.` and a successful resync, all within seconds.
Emulator: the replay reports `Check timed out after 60.0s` — the operation does not answer within the budget, so neither the refusal nor the success is observable. Hypothesis: the handler blocks rather than returning.
Why: a client calling ResyncMFADevice hangs instead of getting the modeled result.
Fix: the ResyncMFADevice handler.
Traces: iam.op-contract-resync-mfa-device.

## Evidence
Cause 1: `AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}}` for both TagInstanceProfile and TagPolicy with a duplicated tag key.
Cause 2: `AssertionError: expected message 'Tags not supported for policies in this domain', got 'Policy arn:aws:iam::aws:policy/ReadOnlyAccess is an AWS managed policy, which cannot be tagged.'`
Cause 3: `AssertionError: expected message 'The number of tags has reached the maximum limit.', got 'Cannot assign more than 50 tags to IAM user sim2real-behav-tagcap-c8e5b095bac1'`
Cause 4: `AssertionError: expected message "1 validation error detected: Value at 'status' failed to satisfy constraint: Member must satisfy enum value set: [Expired, Active, Inactive]", got "... [Active, Inactive, Expired]"`
Cause 5: `Error: Check timed out after 60.0s`

## Steps to Reproduce
Cause 1: TagPolicy(PolicyArn=<customer policy>, Tags=[{Key:'k',Value:'a'},{Key:'k',Value:'b'}]) and the TagInstanceProfile equivalent. Observed 200; expected InvalidInput.
Cause 2: TagPolicy(PolicyArn='arn:aws:iam::aws:policy/ReadOnlyAccess', Tags=[...]). Compare the message to "Tags not supported for policies in this domain".
Cause 3: TagUser up to 50 tags, re-tag an existing key (must not consume cap), then add a 51st distinct key. Compare the message to "The number of tags has reached the maximum limit."
Cause 4: UpdateSigningCertificate(CertificateId=<id>, Status='Bogus'). Compare the enum value set ordering in the message.
Cause 5: enroll an MFA device and call ResyncMFADevice with a wrong code, then a correct pair. Observed: no answer within 60s; expected the modeled refusal/success.
