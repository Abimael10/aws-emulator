Issue-Source: trace-sweep-divergence
Issue-Trace: rds-start-stop-switchover-error-paths
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-start-stop-switchover-error-paths
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|StartActivityStream|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: rds|StartActivityStream|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|StartActivityStream|4xx->4xx|status
Issue-Key: rds|StopActivityStream|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 852fb652ec4af2dc3d2dc139523083e06c8afdc9

# rds activity-stream error paths on a DB cluster target report the wrong fault and echo a synthesized ARN

> _Note: this issue was identified against the code base at commit `852fb652ec4af2dc3d2dc139523083e06c8afdc9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartActivityStream

## Description
2 net-new causes, one shared fault: the activity-stream operations that take a DB cluster target run their cluster lookup BEFORE parameter validation and then describe the miss with an ARN the emulator synthesized rather than the identifier the request supplied. Cause 1 is the precedence half, cause 2 the message half; both live in the same activity-stream error path.

Cause 1 of 2 — StartActivityStream answers 404 DBClusterNotFoundFault where AWS answers 400 InvalidParameterCombination for a default-RDS-KMS-key request (fixture rds-start-stop-switchover-error-paths, step task#5).
AWS: Smithy: StartActivityStream input requires ResourceArn (String), Mode (ActivityStreamMode) and KmsKeyId (String); modeled errors are DBClusterNotFoundFault (404, awsQueryError code DBClusterNotFoundFault), DBInstanceNotFoundFault (404), InvalidDBClusterStateFault (400), InvalidDBInstanceStateFault (400), KMSKeyNotAccessibleFault (400) and ResourceNotFoundFault (404) — none of which is the code AWS actually returns here. AWS documents that an RDS activity stream must be encrypted with a customer managed KMS key and that the AWS-managed default RDS key is not accepted. The recording shows AWS enforcing that rule at PARAMETER VALIDATION, before any cluster resolution: HTTP 400 with Error.Code 'InvalidParameterCombination' and the message 'Activity Streams cannot use Default RDS KMS key.' That message is entirely fixed text — it has no variable parts, so it must be reproduced verbatim including the trailing period. The ordering (KmsKeyId rule checked before the cluster lookup) is recorded behavior: the step's target cluster does not exist, yet AWS still returns the 400, and the sibling step task#6 confirms the same cluster resolves to a not-found.
Emulator: it resolves the cluster target first and never reaches the KmsKeyId rule, answering HTTP 404 with Error.Code 'DBClusterNotFoundFault' and its own sentence 'DBCluster not found: <synthesized cluster ARN>'.
Why: a client asking for an activity stream with the default RDS key is told its cluster is missing instead of that its key choice is invalid. The two errors demand opposite handling: 400 InvalidParameterCombination means fix the KmsKeyId and retry, while 404 DBClusterNotFoundFault drives create-if-absent / treat-as-deleted branches, so a caller provisions or tears down a cluster in response to a key-selection mistake. The default-key guard is also untestable against the emulator, so nothing exercises the one rule that makes activity-stream encryption safe.
Fix: StartActivityStream's input validation in the rds crate — evaluate the KmsKeyId rule (reject the account's default/AWS-managed RDS key with 400 InvalidParameterCombination and AWS's fixed wording) before the ResourceArn target lookup, so parameter validation precedes resolution. The same precedence belongs on the sibling activity-stream operations that share this path.
Traces: rds-start-stop-switchover-error-paths#task#5.

Cause 2 of 2 — the DBCluster-not-found message on StopActivityStream reports a synthesized ARN instead of the identifier the request supplied (fixture rds-start-stop-switchover-error-paths, step task#6).
AWS: Smithy: StopActivityStream input requires ResourceArn (String); modeled errors include DBClusterNotFoundFault (404, awsQueryError code DBClusterNotFoundFault). Status and error code are already correct here; only the message diverges. Recorded behavior for the message: AWS words this fault 'DBCluster not found: <target>' where the fixed text is 'DBCluster not found: ' and the single variable part is the cluster target exactly as the request submitted it — the bare DB cluster identifier when the caller sent an identifier. AWS does not re-render the target as an ARN. The particular identifier in the recording is one run's value filling that slot, not part of the template.
Emulator: it normalizes the submitted target into a full ARN of the form 'arn:aws:rds:<region>:<account>:cluster:<identifier>' and substitutes that into the message, emitting 'DBCluster not found: arn:aws:rds:<region>:<account>:cluster:<identifier>' for a request that named the identifier alone. The same substitution appears in Cause 1's message, which is why both causes are one fix.
Why: the error message is the only place the fault names which target was not found, and clients and tooling parse or log it to decide what to retry or report. Echoing an account- and Region-qualified ARN the caller never sent makes that text unmatchable against the submitted identifier, breaks message-based assertions, and leaks a synthesized account id and Region into an error about a resource the caller addressed by name.
Fix: the rds crate's not-found error construction for cluster-targeted activity-stream operations (the serializer/formatter that builds DBClusterNotFoundFault's Message) — interpolate the target as received from ResourceArn rather than the ARN the resolver built internally, keeping the fixed text 'DBCluster not found: ' unchanged.
Traces: rds-start-stop-switchover-error-paths#task#6.

Related but NOT covering these: open issue 20261119T221450Z-rds-modifyactivitystream-refuses-a-cross-region-resourcearn-as.md reports the same validation-after-lookup inversion for ModifyActivityStream with a cross-Region ARN (404 DBInstanceNotFound instead of 400 InvalidParameterValue). It carries neither of the replay keys above and names a different operation, target kind and rule, so neither cause here is covered by it; a fix for this issue should align the whole activity-stream family on validate-then-resolve.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout; both clusters were reported net-new (no owning issue, no blocking stub).

Cause 1 — fixture rds-start-stop-switchover-error-paths, step task#5, operation StartActivityStream: status expected 400, got 404. $.ErrorResponse.Error.Code: expected 'InvalidParameterCombination', got 'DBClusterNotFoundFault'. $.ErrorResponse.Error.Message: expected 'Activity Streams cannot use Default RDS KMS key.' (all fixed text, no variable parts), got 'DBCluster not found: arn:aws:rds:us-west-2:000000000000:cluster:sim2real-probe-rds-clus-71851fc5'. Divergence key: service=rds|StartActivityStream|status=400->404|keys=$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message,status. affected_count 1.

Cause 2 — fixture rds-start-stop-switchover-error-paths, step task#6, operation StopActivityStream: status 404 expected and returned, Error.Code correct; $.ErrorResponse.Error.Message: expected 'DBCluster not found: sim2real-probe-rds-clus-71851fc5', got 'DBCluster not found: arn:aws:rds:us-west-2:000000000000:cluster:sim2real-probe-rds-clus-71851fc5'. The requirement is the fixed text 'DBCluster not found: ' followed by the target as submitted; the cluster identifier, the Region 'us-west-2' and the account '000000000000' in the quoted strings are this run's values, and the defect is that an ARN appears where the submitted identifier belongs. Divergence key: service=rds|StopActivityStream|status=404->404|keys=$.ErrorResponse.Error.Message. affected_count 1.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, then replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture rds-start-stop-switchover-error-paths step task#5 — POST Action=StartActivityStream to the RDS query endpoint with a cluster ResourceArn, Mode and a KmsKeyId naming the account's default/AWS-managed RDS key. Observe HTTP 404 with Error.Code 'DBClusterNotFoundFault' and 'DBCluster not found: arn:aws:rds:<region>:<account>:cluster:<identifier>' where the recording has HTTP 400 with Error.Code 'InvalidParameterCombination' and 'Activity Streams cannot use Default RDS KMS key.'
3. Cause 2: replay fixture rds-start-stop-switchover-error-paths step task#6 — POST Action=StopActivityStream naming a DB cluster that does not exist, addressing it by bare cluster identifier. Observe the correct HTTP 404 / DBClusterNotFoundFault but the message 'DBCluster not found: arn:aws:rds:<region>:<account>:cluster:<identifier>' where the recording has 'DBCluster not found: <identifier>' echoing the target as submitted.
4. Re-replay both steps (rds-start-stop-switchover-error-paths#task#5 and #task#6) to confirm a fix: closing only one leaves the other diverging.
