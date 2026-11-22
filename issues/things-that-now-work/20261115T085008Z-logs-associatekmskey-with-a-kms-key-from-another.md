Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.associate-kms-key-cross-region-mismatch-rejected
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.associate-kms-key-cross-region-mismatch-rejected|run#11|AssertionError|assert code == "InvalidParameterException", f"expected InvalidParameterException, got {code}"
Issue-Commit: c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9

# logs: AssociateKmsKey with a KMS key from another region is refused AccessDeniedException instead of InvalidParameterException "The specified KMS Key Id could not be found."

> _Note: this issue was identified against the code base at commit `c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateKmsKey

## Description
One net-new root cause. Cause count: 1 — it must be closed.

Cause 1 of 1: AssociateKmsKey does not detect that the supplied kmsKeyId belongs to a different region than the log group, and falls through to its authorization/usability branch, so a cross-region key ARN is refused with the wrong error class and the wrong message.

AWS: AssociateKmsKey resolves kmsKeyId in the log group's own region. A key ARN whose region component is not the log group's region can never be found there, so AWS refuses the call HTTP 400 with error code InvalidParameterException and the message "The specified KMS Key Id could not be found." — that sentence is entirely fixed text, with no variable part (the rejected ARN is NOT echoed). Smithy (services/service-com-amazonaws-cloudwatchlogs-logs-20140328): AssociateKmsKey input kmsKeyId @required @length(0,256), logGroupName @length(1,512) @pattern ^[\.\-_/#A-Za-z0-9]+$; declared errors are InvalidParameterException (client, HTTP 400), OperationAbortedException, ResourceNotFoundException, ServiceUnavailableException — AccessDeniedException is NOT among them for this operation. The refusal must also be effect-free: the log group's kmsKeyId stays unset, and a key from the log group's OWN region is still accepted and becomes readable as kmsKeyId on DescribeLogGroups (positive control in the same check).

Emulator: answers a 4xx carrying error code AccessDeniedException for the cross-region key ARN. The check asserts the code first and fails with "expected InvalidParameterException, got AccessDeniedException"; the message assertion against the exact text "The specified KMS Key Id could not be found." is therefore not even reached, so the message wording on this path is unverified and must be set to AWS's fixed sentence as part of the fix.

Why: a client branches on the error code to decide what to do. InvalidParameterException tells it the request named a key that does not exist in this region — fix the ARN (usually a region typo) and retry. AccessDeniedException tells it the key exists but the caller/service is not permitted to use it — the operator is sent to edit a KMS key policy or add a grant for a key that is not even in the right region, and automated remediation that retries only on authorization failures loops instead of surfacing a malformed request. Error-class-driven SDK retry/classification logic (AccessDenied is commonly surfaced as a permissions incident) also mis-triages the failure.

Fix: the kmsKeyId resolution branch of the AssociateKmsKey handler in the CloudWatch Logs crate — parse the ARN and compare its region to the target log group's region BEFORE the key-usability/authorization check, and on mismatch raise InvalidParameterException with AWS's fixed message "The specified KMS Key Id could not be found.", leaving the log group's kmsKeyId unset. The same-region accept path must remain unchanged.

Related but NOT covering this cause: open issue 20261113T223135Z-logs-anomaly-detector-and-kms-key-paths-diverge.md (its cause 2) covers the DIFFERENT input class of a key that does not exist / is not usable with the log group in the SAME region, where the recording shows AccessDeniedException with the message "The specified KMS key does not exist or is not allowed to be used with Arn '<log group arn>'"; closing it does not make the cross-region ARN classify as InvalidParameterException and, taken alone, would entrench the AccessDeniedException seen here. Open issue 20261108T145640Z-logs-9-net-new-causes-cloudwatch-logs-insights.md carries AssociateKmsKey selector (logGroupName/resourceIdentifier) validation, a different branch of the same handler. Neither carries this check's replay key.

Traces: behavioral check logs.associate-kms-key-cross-region-mismatch-rejected.

## Evidence
Failing behavioral check logs.associate-kms-key-cross-region-mismatch-rejected (verdict: mismatch).

Assertion from the replay:
  File "check_logs_associate_kms_key_cross_region_mismatch_rejected.py", line 39, in run
    assert code == "InvalidParameterException", f"expected InvalidParameterException, got {code}"
  AssertionError: expected InvalidParameterException, got AccessDeniedException

Expected (AWS): HTTP 400, error code InvalidParameterException, message exactly "The specified KMS Key Id could not be found." (all fixed text, no request- or state-derived values), the log group left with no kmsKeyId, and a same-region key ARN subsequently accepted and reported back by DescribeLogGroups.
Actual (emulator): a 4xx whose error code is AccessDeniedException; because the code assertion fires first, the message is not compared.

The specific region pair used by the check (ctx.region vs us-west-2/us-east-1) and the generated log-group and key names/ARNs are per-run values that merely exercise the mismatch; the requirement is the error code and the fixed message text for ANY kmsKeyId whose ARN region differs from the log group's region.

## Steps to Reproduce
1. CreateLogGroup(logGroupName=<name>) in region R.
2. In region R, KMS CreateKey (SYMMETRIC_DEFAULT / ENCRYPT_DECRYPT) and add a key-policy statement allowing logs.<R>.amazonaws.com to Encrypt/Decrypt/ReEncrypt*/GenerateDataKey*/Describe* — this is the positive control.
3. In a DIFFERENT region R2 (any region other than R), KMS CreateKey and take its key ARN.
4. Call logs AssociateKmsKey(logGroupName=<name>, kmsKeyId=<the R2 key ARN>).
   Expect: HTTP 400, error code InvalidParameterException, Message "The specified KMS Key Id could not be found."
   Observed: refused with error code AccessDeniedException.
5. DescribeLogGroups(logGroupNamePrefix=<name>) — the group must still report no kmsKeyId.
6. Call AssociateKmsKey(logGroupName=<name>, kmsKeyId=<the region-R key ARN>) and poll DescribeLogGroups until kmsKeyId equals that ARN (positive control must keep passing).

Replay: behavioral check logs.associate-kms-key-cross-region-mismatch-rejected.
