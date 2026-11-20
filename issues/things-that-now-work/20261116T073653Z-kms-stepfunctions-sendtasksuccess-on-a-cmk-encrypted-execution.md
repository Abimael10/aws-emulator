Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__stepfunctions.send-task-success-cmk-role-without-decrypt-denied-sourcearn-trust-works
Issue-Key: kms__stepfunctions.send-task-success-cmk-role-without-decrypt-denied-sourcearn-trust-works|_error#18|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 5636e5ddac188567344946aa5c15e82054f1c023

# kms__stepfunctions: SendTaskSuccess on a CMK-encrypted execution succeeds (HTTP 200) after the execution role's kms:Decrypt grant is revoked — no KmsAccessDeniedException is ever raised

> _Note: this issue was identified against the code base at commit `5636e5ddac188567344946aa5c15e82054f1c023`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendTaskSuccess

## Description
Cause 1 of 1: the callback path (SendTaskSuccess on an outstanding activity task token) performs no KMS authorization at all for a CUSTOMER_MANAGED_KMS_KEY state machine, so the call is accepted with an empty 200 body even when the execution role's identity policy no longer allows kms:Decrypt on the machine's key.
Failing check id: kms__stepfunctions.send-task-success-cmk-role-without-decrypt-denied-sourcearn-trust-works.

AWS: Smithy (services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json) models SendTaskSuccess{taskToken (required, @length 1..2048, @sensitive), output (required, @length 0..262144, @sensitive)} -> empty output, errors InvalidOutput | InvalidToken | KmsAccessDeniedException | KmsInvalidStateException | KmsThrottlingException | TaskDoesNotExist | TaskTimedOut, all HTTP 400. KmsAccessDeniedException is documented as "Either your KMS key policy or API caller does not have the required permissions." Step Functions must decrypt the execution's data key to accept and store the task result, so for a machine whose encryptionConfiguration.type is CUSTOMER_MANAGED_KMS_KEY the call is authorized as the execution role's STS session against the key: once that session is no longer allowed kms:Decrypt on the key, SendTaskSuccess is refused HTTP 400 KmsAccessDeniedException whose message is the relayed KMS denial. Its fixed text is "User: <principal> is not authorized to perform: kms:Decrypt on resource: <key-arn> because no identity-based policy allows the kms:Decrypt action (Service: Kms, Status Code: 400, Request ID: <request-id>) (SDK Attempt Count: <n>)" — every word of that is fixed; the variable parts are <principal>, the execution role's assumed-role session ARN arn:aws:sts::<account>:assumed-role/<role-name>/<session> taken from the machine's roleArn and the session Step Functions mints, <key-arn> taken from the machine's encryptionConfiguration.kmsKeyId, and the per-call <request-id> / <n>. Restoring the grant must make the SAME still-outstanding token usable again (the token is not invalidated by the denial), after which the execution reaches SUCCEEDED with the worker's output relayed byte-for-byte. Recorded behavior confirms both legs.

Emulator: SendTaskSuccess answers HTTP 200 with an empty body ({'ResponseMetadata': {'HTTPStatusCode': 200, 'content-length': '2', ...}}) for a token of a CMK-encrypted execution whose execution role's inline policy was just rewritten to allow only kms:GenerateDataKey (kms:Decrypt removed), 150 s earlier. No error code is produced at all — neither KmsAccessDeniedException nor any other modeled error — so the KMS grant of the callback path is not consulted; only the token's existence appears to be checked.

Why: a client relying on AWS's behavior gets the opposite of the security answer. Revoking kms:Decrypt from a state machine's execution role is the documented way to cut off an encrypted machine's ability to read and write execution data; on the emulator the revocation is a no-op for callbacks, so task results are still accepted and persisted under a key the principal is no longer authorized to use. Negative tests for that revocation pass vacuously, and any code that distinguishes a permanent KMS denial from a transient propagation window (retry on KmsAccessDeniedException, stop otherwise) never sees the denial and silently consumes the one-shot task token.

Fix: the SendTaskSuccess handler's KMS authorization step for executions of CUSTOMER_MANAGED_KMS_KEY state machines — it must authorize the data-key decrypt as the execution role's assumed-role session against both the key policy and the role's identity policies, refuse HTTP 400 KmsAccessDeniedException with the relayed KMS message when kms:Decrypt is not allowed, and leave the task token outstanding so the same token still works once the grant is restored. The same authorization belongs on the sibling callback operations (SendTaskFailure, SendTaskHeartbeat), which model the identical KMS error set.

Neighbouring open issues that do NOT cover this (the report left this check unmarked — no ALREADY FILED, no OWNED): 20261113T123838Z-kms-stepfunctions-3-net-new-causes-startexecution-on.md (the StartExecution data-key path, which is over-refusing, not this under-refusing callback path), 20261111T062437Z-kms-stepfunctions-3-net-new-causes-updatestatemachine-back.md (UpdateStateMachine/mid-execution key state), 20261109T171600Z-stepfunctions-step-functions-the-accessdeniedexception-for-states-sendtasksuccess-names.md (the IAM states:SendTaskSuccess denial's resource field, not the KMS grant), and 20261113T224249Z-stepfunctions-every-tasktimedout-refusal-sendtasksuccess-sendtaskfailure-sendtaskheartbeat-substitutes.md (TaskTimedOut wording).

Traces: kms__stepfunctions.send-task-success-cmk-role-without-decrypt-denied-sourcearn-trust-works.

## Evidence
Check kms__stepfunctions.send-task-success-cmk-role-without-decrypt-denied-sourcearn-trust-works, verdict mismatch.

Setup performed by the check: kms:CreateKey (ENCRYPT_DECRYPT, default key policy); iam:CreateRole trusting states.amazonaws.com with Condition StringEquals {aws:SourceArn: <state-machine ARN>, aws:SourceAccount: <account>}; iam:PutRolePolicy "kms" allowing [kms:Decrypt, kms:GenerateDataKey] on the key ARN; sfn:CreateActivity; sfn:CreateStateMachine(type=STANDARD, a single activity Task with TimeoutSeconds=1200, encryptionConfiguration={type: CUSTOMER_MANAGED_KMS_KEY, kmsKeyId: <key ARN>, kmsDataKeyReusePeriodSeconds: 60}); sfn:StartExecution; sfn:GetActivityTask to obtain the outstanding taskToken. Then iam:PutRolePolicy rewrites the same inline policy to allow only [kms:GenerateDataKey] (kms:Decrypt revoked) and the check waits 150 s before making ONE determinative call.

Expected (AWS): sfn:SendTaskSuccess(taskToken=<token>, output='{"ok": true}') is refused HTTP 400 with Error.Code "KmsAccessDeniedException" and Error.Message 'User: arn:aws:sts::<account>:assumed-role/<role-name>/<session> is not authorized to perform: kms:Decrypt on resource: <key ARN> because no identity-based policy allows the kms:Decrypt action (Service: Kms, Status Code: 400, Request ID: <request-id>) (SDK Attempt Count: <n>)'. The check masks exactly <session>, <request-id> and <n>; everything else in that string is fixed text or a value from the request/state (role name from the machine's roleArn, key ARN from its encryptionConfiguration).

Actual (emulator): the call SUCCEEDED. The harness reports:
  AssertionError: expected the call to be refused, but it succeeded: {'ResponseMetadata': {'RequestId': '<id>', 'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.0', 'x-amzn-requestid': '<id>', 'content-length': '2', 'date': '...'}, 'RetryAttempts': 0}}
raised from the check's _error() helper at line 255 (called from run() line 74). HTTP 200 with a 2-byte ({}) body: no error code, no KMS authorization. Because the refusal never happens, the rest of the scenario (restore kms:Decrypt, reuse the SAME outstanding token, execution SUCCEEDED with output relayed and the non-ASCII input read back byte-for-byte) is never reached.

## Steps to Reproduce
1. kms:CreateKey (KeyUsage=ENCRYPT_DECRYPT), leaving the default key policy (root delegation) in place.
2. iam:CreateRole trusting Service states.amazonaws.com for sts:AssumeRole, with Condition StringEquals {"aws:SourceArn": "arn:aws:states:<region>:<account>:stateMachine:<name>", "aws:SourceAccount": "<account>"}; iam:PutRolePolicy(PolicyName="kms") allowing Action ["kms:Decrypt", "kms:GenerateDataKey"] on the key ARN.
3. sfn:CreateActivity(name=<act>).
4. sfn:CreateStateMachine(name=<name>, type=STANDARD, roleArn=<role>, definition={"StartAt":"T","States":{"T":{"Type":"Task","Resource":"<activity ARN>","TimeoutSeconds":1200,"End":true}}}, encryptionConfiguration={"type":"CUSTOMER_MANAGED_KMS_KEY","kmsKeyId":"<key ARN>","kmsDataKeyReusePeriodSeconds":60}).
5. sfn:StartExecution(stateMachineArn=<sm>, input='{"marker":"...","note":"\u00fcn\u00efcode \u2713"}'); sfn:GetActivityTask(activityArn=<act>) until a taskToken is returned.
6. iam:PutRolePolicy(RoleName=<role>, PolicyName="kms", PolicyDocument allowing only ["kms:GenerateDataKey"] on the key ARN) — i.e. revoke kms:Decrypt. Wait for the revocation to take effect.
7. sfn:SendTaskSuccess(taskToken=<token>, output='{"ok": true}').
   Expected: HTTP 400 KmsAccessDeniedException, message 'User: arn:aws:sts::<account>:assumed-role/<role-name>/<session> is not authorized to perform: kms:Decrypt on resource: <key ARN> because no identity-based policy allows the kms:Decrypt action (Service: Kms, Status Code: 400, Request ID: <request-id>) (SDK Attempt Count: <n>)'.
   Observed: HTTP 200 with an empty JSON body.
8. Restore the policy with kms:Decrypt and call sfn:SendTaskSuccess again with the SAME token: it must now succeed, and DescribeExecution must report status SUCCEEDED, output {"ok": true} relayed exactly, and input equal byte-for-byte to what was sent in step 5.
