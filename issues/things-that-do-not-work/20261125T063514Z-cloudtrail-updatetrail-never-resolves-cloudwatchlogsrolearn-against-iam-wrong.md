Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail.create-trail-cloudwatch-logs-arn-validation cloudtrail.put-insight-selectors-rejects-insufficient-kms-policy
Issue-Shape-Id: com.amazonaws.cloudtrail#CloudTrail_20131101
Issue-Key: cloudtrail.create-trail-cloudwatch-logs-arn-validation|run#20|AssertionError|assert code == "InvalidCloudWatchLogsRoleArnException", code
Issue-Key: cloudtrail.put-insight-selectors-rejects-insufficient-kms-policy|run#34|AssertionError|raise AssertionError(
Issue-Commit: b381b9dbba4fbb02bac11f8b7bd070a116c82713

# cloudtrail: UpdateTrail never resolves CloudWatchLogsRoleArn against IAM (wrong refusal when both halves are absent), and PutInsightSelectors never re-validates the trail's KMS key policy (2 causes)

> _Note: this issue was identified against the code base at commit `b381b9dbba4fbb02bac11f8b7bd070a116c82713`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateTrail

## Description
Two net-new root causes in the CloudTrail trail-configuration surface, both grounded in failing behavioral checks of the `cloudtrail` service.

Cause 1 of 2: a CloudWatch Logs pair whose ROLE does not exist is refused on the LOG GROUP instead of on the role.
AWS: Smithy (services/service-com-amazonaws-cloudtrail-cloudtrail-20131101) models both refusals on UpdateTrail and CreateTrail: InvalidCloudWatchLogsRoleArnException, HTTP 400, awsQueryError code InvalidCloudWatchLogsRoleArn, doc "This exception is thrown when the provided role is not valid"; and InvalidCloudWatchLogsLogGroupArnException, HTTP 400, code InvalidCloudWatchLogsLogGroupArn, doc "thrown when the provided CloudWatch Logs log group is not valid". UpdateTrailRequest$CloudWatchLogsRoleArn documents "You must use a role that exists in your account" and $CloudWatchLogsLogGroupArn "You must use a log group that exists in your account". Recorded behavior: CloudTrail resolves the ROLE half FIRST and against IAM, not merely against ARN syntax, so an UpdateTrail naming a well-formed in-account role ARN that names NO existing role together with a log-group ARN that names no existing log group is refused with code InvalidCloudWatchLogsRoleArnException and the message "Access denied. Verify in IAM that the role has adequate trust relationships." — that whole sentence is fixed text for this error class, with no variable part. The same call with a REAL role and the same absent log group is refused InvalidCloudWatchLogsLogGroupArnException / "You must specify an existing log group." (also entirely fixed text), and that leg does pass today, which is what isolates the ordering/existence defect to the role half.
Emulator: the role half is only checked SYNTACTICALLY — service_common::arn::IAM_ROLE.admits(role) plus an account match — so a well-formed but non-existent role ARN is accepted as the role half, evaluation falls through to the log group, and the call is refused 400 InvalidCloudWatchLogsLogGroupArnException ("You must specify an existing log group."). The role's existence and its trust relationship with cloudtrail.amazonaws.com are never evaluated, so InvalidCloudWatchLogsRoleArnException is reachable only for the half-missing and malformed-ARN cases, never for a role that does not exist.
Why: a client wiring CloudWatch Logs delivery is told the wrong half of its configuration is at fault. Automation that branches on the error code (retry/re-create the log group vs. fix the IAM role and its trust policy) fixes the wrong resource and loops forever, and a tool that only checks "the role ARN was accepted" concludes a non-existent role is usable.
Fix: the CloudWatch Logs resolution shared by CreateTrail and UpdateTrail must resolve the role half in IAM (role exists in this account, and its trust policy admits cloudtrail.amazonaws.com) and return the role refusal before the log-group half is judged — i.e. the role branch of the trail's referenced-resource resolver, not the operation handlers' error mapping.
Traces: cloudtrail.create-trail-cloudwatch-logs-arn-validation — the assertion that fails is the UpdateTrail leg with an absent-but-well-formed role ARN plus the absent log group (code InvalidCloudWatchLogsRoleArnException expected, InvalidCloudWatchLogsLogGroupArnException observed). Code: services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/src/state/referenced_resources.rs lines 384-423 (cloud_watch_logs_verdict — role judged by ARN syntax only, then LogGroupAbsent), consumed at services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/src/ops/update_trail.rs lines 667-693.

Cause 2 of 2: PutInsightSelectors never re-validates the trail's KMS key policy, so InsufficientEncryptionPolicyException is unreachable and a call AWS refuses is accepted.
AWS: Smithy models InsufficientEncryptionPolicyException on PutInsightSelectors, HTTP 400, awsQueryError code InsufficientEncryptionPolicy, documented as "For the CreateTrail PutInsightSelectors, UpdateTrail, StartQuery, and StartImport operations, this exception is thrown when the policy on the S3 bucket or KMS key does not have sufficient permissions for the operation." Recorded behavior: for a trail created with KmsKeyId, after the key policy is rewritten to drop CloudTrail's kms:GenerateDataKey*/kms:DescribeKey grants, PutInsightSelectors is refused 400 InsufficientEncryptionPolicyException with the message "Insufficient permissions to access S3 bucket <bucket> or KMS key <key ARN>." — fixed text "Insufficient permissions to access S3 bucket " / " or KMS key " / "." with the bucket name and the fully specified key ARN taken from the trail's own configuration — and the stored insight selectors are left unchanged; restoring the policy makes the identical call succeed.
Emulator: PutInsightSelectors resolves ONLY the S3 destination (bucket existence and bucket policy) and never the trail's KMS key policy, so after the grants are revoked the call is still accepted 200 and the selectors are mutated (the check observes no refusal at all, 45s after PutKeyPolicy returned). No InsufficientEncryptionPolicyException is reachable from this operation for a KMS-policy cause.
Why: the emulator is overly permissive on an encryption-policy regression — a consumer validating that revoking CloudTrail's key grants blocks further Insights configuration sees the opposite, and a control that depends on the refusal (break-glass key-policy revocation) silently has no effect, while the selectors change despite the missing permissions.
Fix: PutInsightSelectors' referenced-resource resolution must ask the KMS key-policy question for the trail's KmsKeyId alongside the bucket question, and refuse with the modelled InsufficientEncryptionPolicyException (naming both the bucket and the key ARN) BEFORE any selector is written.
Traces: cloudtrail.put-insight-selectors-rejects-insufficient-kms-policy. Code: services/service-com-amazonaws-cloudtrail-cloudtrail-20131101/src/ops/put_insight_selectors.rs lines 296-394 (resolve_destination handles only DestinationVerdict::{NoSuchBucket,PolicyInsufficient}; no key-policy branch).

## Evidence
Replay report, suite simparity-behavioral-aws-v1, service heading `### cloudtrail` (96/102, 6 failed). Neither check is marked ALREADY FILED or OWNED.

Cause 1 — [mismatch] cloudtrail.create-trail-cloudwatch-logs-arn-validation:
  File "check_cloudtrail_create_trail_cloudwatch_logs_arn_validation.py", line 246, in run
    assert code == "InvalidCloudWatchLogsRoleArnException", code
  AssertionError: InvalidCloudWatchLogsLogGroupArnException
Expected (AWS): HTTP 400, Error.Code InvalidCloudWatchLogsRoleArnException, Error.Message "Access denied. Verify in IAM that the role has adequate trust relationships." for UpdateTrail(Name=<trail>, CloudWatchLogsLogGroupArn=<in-account in-region log-group ARN naming nothing>, CloudWatchLogsRoleArn=<in-account role ARN naming nothing>).
Actual (emulator): HTTP 400, Error.Code InvalidCloudWatchLogsLogGroupArnException with the log-group message. The immediately preceding leg of the same check — the SAME absent log group with a REAL role — passes with InvalidCloudWatchLogsLogGroupArnException / "You must specify an existing log group.", so the only difference between the passing and failing legs is whether the role exists.

Cause 2 — [mismatch] cloudtrail.put-insight-selectors-rejects-insufficient-kms-policy:
  File "check_cloudtrail_put_insight_selectors_rejects_insufficient_kms_policy.py", line 165, in run
  AssertionError: PutInsightSelectors did not re-validate the revoked KMS key policy even 45s after PutKeyPolicy returned
Expected (AWS): after kms:PutKeyPolicy reduces the key policy to the account-root statement only, PutInsightSelectors(TrailName=<trail>, InsightSelectors=[ApiCallRateInsight, ApiErrorRateInsight]) is refused HTTP 400 InsufficientEncryptionPolicyException, message "Insufficient permissions to access S3 bucket <bucket> or KMS key <key ARN>.", and GetInsightSelectors still reports only [ApiCallRateInsight].
Actual (emulator): the call is ACCEPTED (no ClientError raised in either attempt of the 25s+20s window), so no refusal, no error code and no message exist to compare. Setup in the same check succeeds: CreateTrail with KmsKeyId echoes the fully specified key ARN, GetTrail reports it, and the first PutInsightSelectors round-trips — the divergence is isolated to the missing key-policy re-validation.

## Steps to Reproduce
Cause 1:
1. Boot the emulator. Create an S3 bucket with the CloudTrail delivery policy and CreateTrail(Name=T, S3BucketName=B).
2. Create an IAM role R with a trust policy for cloudtrail.amazonaws.com (so one leg has a real role).
3. UpdateTrail(Name=T, CloudWatchLogsLogGroupArn="arn:aws:logs:<region>:<account>:log-group:<name that does not exist>:*", CloudWatchLogsRoleArn=<ARN of R>) → observe 400 InvalidCloudWatchLogsLogGroupArnException "You must specify an existing log group." (correct today).
4. Repeat with CloudWatchLogsRoleArn="arn:aws:iam::<account>:role/<role that does not exist>" and the SAME absent log-group ARN. AWS answers 400 InvalidCloudWatchLogsRoleArnException "Access denied. Verify in IAM that the role has adequate trust relationships."; the emulator answers 400 InvalidCloudWatchLogsLogGroupArnException.
5. GetTrail(Name=T) must still report neither CloudWatchLogsLogGroupArn nor CloudWatchLogsRoleArn.

Cause 2:
1. Create an S3 bucket with the CloudTrail delivery policy, and a KMS CMK whose policy grants account root kms:*, plus cloudtrail.amazonaws.com kms:GenerateDataKey* (conditioned on aws:SourceArn=<trail ARN>, aws:SourceAccount=<account>) and kms:DescribeKey.
2. CreateTrail(Name=T, S3BucketName=B, KmsKeyId=<key id>) — the response echoes the full key ARN.
3. PutInsightSelectors(TrailName=T, InsightSelectors=[{InsightType: ApiCallRateInsight}]) → succeeds; GetInsightSelectors confirms it.
4. kms:PutKeyPolicy(KeyId=<key id>, PolicyName=default, Policy=<account-root statement only>), dropping every CloudTrail grant.
5. PutInsightSelectors(TrailName=T, InsightSelectors=[ApiCallRateInsight, ApiErrorRateInsight]). AWS refuses 400 InsufficientEncryptionPolicyException "Insufficient permissions to access S3 bucket <bucket> or KMS key <key ARN>." and leaves the selectors at [ApiCallRateInsight]; the emulator accepts it and stores both.
6. Restore the full key policy and repeat step 5: the identical call must now succeed and GetInsightSelectors must report both insight types.
